//! Голосовой ввод: запись из панели распознаётся на этой машине.
//!
//! Модель — GigaAM v3 (Сбер, MIT) через ONNX Runtime. На CPU она точнее Whisper
//! на русском и в разы быстрее: 10 секунд речи распознаются примерно за секунду.
//! Модель (~300 МБ) скачивается один раз при первой записи в каталог агента,
//! дальше распознавание идёт без сети — запись не покидает эту машину.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use aipanel_proto::MAX_AUDIO_BYTES;
use gigastt_core::{
    inference::{Engine, TranscribeRequest, TranscribeSource},
    model::{self, ModelVariant},
};
use tokio::sync::Mutex;

use crate::util;

/// Частота сэмплов декодера: `decode_audio_bytes` всегда отдаёт моно 16 кГц.
const SAMPLE_RATE: usize = 16_000;
/// Пик, ниже которого запись считается тишиной (−40 dBFS от полной шкалы).
/// Модель на тишине выдаёт правдоподобные, но выдуманные слова, поэтому такую
/// запись лучше не распознавать вовсе.
const VOICE_MIN_PEAK: f32 = 0.01;
/// Сколько в записи должно быть звука выше порога, чтобы её стоило слушать.
const VOICE_MIN_SPEECH_S: f64 = 0.15;
/// Какая доля записи должна быть речью. Телефон иногда отдаёт микрофон,
/// который замолкает через полсекунды: в записи секунды тишины и двести
/// миллисекунд голоса, и модель на таком обрезке выдаёт одно-два выдуманных
/// слова. Живая диктовка с паузами даёт заметно больше, поэтому порог низкий:
/// он отделяет «звука почти нет» от «человек сказал одно слово».
const VOICE_MIN_SPEECH_RATIO: f64 = 0.08;

pub struct Voice {
    enabled: bool,
    variant: ModelVariant,
    /// Слова, на которые модель должна настраиваться: латиница и имена.
    terms: Vec<String>,
    /// Замены в готовом тексте: что модель пишет и что должно стоять вместо.
    fix: BTreeMap<String, String>,
    dir: PathBuf,
    /// Загруженный движок: `None` — ещё не грузили. Загрузка одна на процесс:
    /// параллельные записи ждут её под этим же замком.
    engine: Mutex<Option<Arc<Engine>>>,
}

impl Voice {
    pub fn new(cfg: &crate::config::VoiceCfg) -> Self {
        // Модель живёт в каталоге агента рядом с избранным и подписками: у
        // нескольких агентов на одной машине (`--home`) она своя.
        let dir = util::aipanel_dir().join("models");
        let variant = match cfg.model.as_str() {
            "rnnt" => ModelVariant::Rnnt,
            _ => ModelVariant::E2eRnnt,
        };
        // Свои замены дополняют встроенные, а не заменяют их: человек, дописавший
        // одну пару в `[voice.fix]`, не должен потерять остальные.
        let mut fix = crate::config::VoiceCfg::default().fix;
        fix.extend(cfg.fix.clone());
        Self {
            enabled: cfg.enabled,
            variant,
            terms: cfg.terms.clone(),
            fix,
            dir,
            engine: Mutex::new(None),
        }
    }

    /// Распознаёт запись из панели: base64 в том виде, в каком его отдал браузер
    /// (Opus в WebM/Ogg или AAC в MP4 — формат определяет декодер).
    pub async fn transcribe(&self, audio: &str) -> Result<String, String> {
        if !self.enabled {
            return Err("голосовой ввод выключен в agent.toml".into());
        }
        let Some(bytes) = aipanel_proto::b64::decode(audio) else {
            return Err("запись повреждена".into());
        };
        if bytes.is_empty() {
            return Err("пустая запись".into());
        }
        if bytes.len() > MAX_AUDIO_BYTES {
            return Err("запись слишком длинная".into());
        }
        let engine = self.engine().await?;
        // Распознавание считает процессор и занимает секунды: цикл сообщений
        // агента не должен ждать его, поэтому работа уходит в отдельный поток.
        let text = tokio::task::spawn_blocking(move || {
            // Запись декодируется один раз: по готовым сэмплам видно, есть ли в
            // них звук, и они же уходят в модель. Телефон иногда присылает файл,
            // в котором речи нет вовсе (микрофон замолчал): модель на тишине
            // выдаёт правдоподобные, но выдуманные слова, и человек принимает их
            // за ошибку распознавания. Честнее сказать, что звука нет. Панель
            // отсекает такое сама, но старый её кэш и чужие клиенты — нет.
            let samples = gigastt_core::inference::audio::decode_audio_bytes(&bytes)
                .map_err(|e| format!("запись не декодировалась: {e}"))?;
            let level = level_of(&samples);
            let secs = samples.len() as f64 / SAMPLE_RATE as f64;
            util::log(&format!(
                "voice: {}, {secs:.2} с, пик {:.4}, речи {:.2} с",
                format_name(&bytes),
                level.peak,
                level.secs
            ));
            if level.peak < VOICE_MIN_PEAK {
                return Err("в записи тишина — микрофон молчит".to_string());
            }
            if level.secs < VOICE_MIN_SPEECH_S {
                return Err("в записи почти нет речи — слишком тихо".to_string());
            }
            // Микрофон, замолчавший через полсекунды, оставляет от фразы
            // обрезок: распознавать его — значит выдать выдуманные слова.
            if secs >= 1.0 && level.secs < secs * VOICE_MIN_SPEECH_RATIO {
                return Err("микрофон почти всю запись молчал — нечего распознавать".to_string());
            }
            let mut guard = engine.pool.checkout_blocking().map_err(|e| format!("движок занят: {e}"))?;
            let request = TranscribeRequest::new(TranscribeSource::Samples(&samples));
            let result = engine.transcribe_request(request, &mut guard).map_err(|e| e.to_string())?;
            Ok::<String, String>(result.text)
        })
        .await
        .map_err(|e| format!("распознавание прервано: {e}"))??;
        // Распознанный текст — в лог: по нему видно, какие слова модель слышит
        // неверно, и что стоит дописать в `[voice.fix]`.
        util::log(&format!("voice: {text}"));
        Ok(self.postprocess(&text))
    }

    /// Приводит текст в вид для поля ввода: убирает лишние пробелы и правит
    /// слова, которые модель услышала по-русски (`карго` → `cargo`).
    fn postprocess(&self, text: &str) -> String {
        let mut out = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for (from, to) in &self.fix {
            out = replace_ignore_case(&out, from, to);
        }
        out
    }

    /// Загруженный движок; при первом вызове скачивает модель и грузит её.
    async fn engine(&self) -> Result<Arc<Engine>, String> {
        let mut slot = self.engine.lock().await;
        if let Some(engine) = slot.as_ref() {
            return Ok(Arc::clone(engine));
        }
        let dir = self.dir.clone();
        std::fs::create_dir_all(&dir).map_err(|e| format!("не удалось создать {}: {e}", dir.display()))?;
        let path = dir.to_string_lossy().into_owned();
        // Проверка перед скачиванием: первый раз это сотни мегабайт, и человек
        // в панели должен видеть, чего он ждёт (подсказку показывает браузер,
        // здесь — только лог).
        if !model::is_prequantized_present(self.variant, &dir) {
            util::log(&format!(
                "voice: скачиваю модель {} в {} — это сотни мегабайт, один раз",
                variant_name(self.variant),
                dir.display()
            ));
        }
        let variant = model::ensure_prequantized_model_variant(Some(self.variant), &path)
            .await
            .map_err(|e| format!("модель не скачалась: {e}"))?;
        let terms = self.terms.clone();
        let threads = std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(1);
        let engine = tokio::task::spawn_blocking(move || {
            let engine = Engine::load_with_pools_threads_variant(&path, Some(variant), 1, 1, 0, threads)
                .map_err(|e| format!("модель не загрузилась: {e}"))?;
            // Смещение декодера к латинице: иначе `cargo` выходит «карго».
            let phrases: Vec<(String, f32)> = terms.into_iter().map(|t| (t, 1.0)).collect();
            let engine = if phrases.is_empty() { engine } else { engine.with_hotwords(&phrases, 2.0) };
            Ok::<Engine, String>(engine)
        })
        .await
        .map_err(|e| format!("загрузка модели прервана: {e}"))??;
        util::log(&format!("voice: модель {} готова", variant_name(self.variant)));
        let engine = Arc::new(engine);
        *slot = Some(Arc::clone(&engine));
        Ok(engine)
    }
}

/// Насколько громкая запись: пик и сколько в ней звука выше порога. По этим
/// числам в логе видно, почему модель не расслышала слова — тишина в записи,
/// обрезанный фрагмент или слишком тихая речь.
struct Level {
    peak: f32,
    secs: f64,
}

fn level_of(samples: &[f32]) -> Level {
    let mut peak = 0.0f32;
    let mut loud = 0usize;
    for sample in samples {
        let level = sample.abs();
        if level > peak {
            peak = level;
        }
        if level >= VOICE_MIN_PEAK {
            loud += 1;
        }
    }
    Level { peak, secs: loud as f64 / SAMPLE_RATE as f64 }
}

/// Контейнер записи по первым байтам — только для лога.
fn format_name(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        "webm"
    } else if bytes.starts_with(b"OggS") {
        "ogg"
    } else if bytes.len() > 8 && &bytes[4..8] == b"ftyp" {
        "mp4"
    } else if bytes.starts_with(b"RIFF") {
        "wav"
    } else {
        "неизвестный контейнер"
    }
}

fn variant_name(variant: ModelVariant) -> &'static str {
    match variant {
        ModelVariant::Rnnt => "GigaAM v3 rnnt",
        ModelVariant::E2eRnnt => "GigaAM v3 e2e_rnnt",
        _ => "GigaAM v3",
    }
}

/// Заменяет `from` на `to` без учёта регистра. Если привести строку к нижнему
/// регистру нельзя без сдвига байтов (редкие языки), остаётся точное совпадение:
/// испортить текст важнее, чем поправить одно слово.
fn replace_ignore_case(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let pattern = from.to_lowercase();
    if lower.len() != text.len() {
        return text.replace(from, to);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(pos) = lower[at..].find(&pattern) {
        let start = at + pos;
        let end = start + pattern.len();
        out.push_str(&text[at..start]);
        out.push_str(to);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_by_case_insensitive_match() {
        let text = "Запусти Карго тест и карго клippy";
        assert_eq!(
            replace_ignore_case(text, "карго", "cargo"),
            "Запусти cargo тест и cargo клippy"
        );
    }

    #[test]
    fn keeps_text_when_nothing_matches() {
        assert_eq!(replace_ignore_case("просто текст", "карго", "cargo"), "просто текст");
    }

    #[test]
    fn ignores_empty_pattern() {
        assert_eq!(replace_ignore_case("текст", "", "x"), "текст");
    }

    /// Запись, в которой речи нет вовсе: пик нулевой, модель слушать нечего.
    #[test]
    fn level_of_silence_is_zero() {
        let level = level_of(&vec![0.0; SAMPLE_RATE]);
        assert_eq!(level.peak, 0.0);
        assert_eq!(level.secs, 0.0);
    }

    /// Полсекунды речи в двух секундах записи: пик по самому громкому сэмплу,
    /// «речи» — сколько звука выше порога тишины.
    #[test]
    fn level_of_counts_speech_above_threshold() {
        let mut samples = vec![0.0f32; SAMPLE_RATE * 2];
        for sample in samples.iter_mut().take(SAMPLE_RATE / 2) {
            *sample = 0.3;
        }
        let level = level_of(&samples);
        assert!((level.peak - 0.3).abs() < 1e-6);
        assert!((level.secs - 0.5).abs() < 0.01);
    }

    /// Запись с телефона, у которого микрофон замолчал через полсекунды: пик
    /// громкий (первый слог дошёл), а речи — десятая доля секунды из двух с
    /// половиной. Именно такую запись нельзя отдавать модели: она ответит
    /// «Про» или другим выдуманным словом.
    #[test]
    fn dead_microphone_is_caught_by_speech_ratio() {
        let secs = 2.5;
        let mut samples = vec![0.0f32; (SAMPLE_RATE as f64 * secs) as usize];
        for sample in samples.iter_mut().take(SAMPLE_RATE / 10) {
            *sample = 0.4;
        }
        let level = level_of(&samples);
        assert!(level.peak > VOICE_MIN_PEAK);
        assert!(level.secs < VOICE_MIN_SPEECH_S);
        assert!(level.secs < secs * VOICE_MIN_SPEECH_RATIO);
    }

    #[test]
    fn format_name_sniffs_container() {
        assert_eq!(format_name(&[0x1a, 0x45, 0xdf, 0xa3, 0x01]), "webm");
        assert_eq!(format_name(b"OggS...."), "ogg");
        assert_eq!(format_name(b"\0\0\0\x20ftypisom"), "mp4");
        assert_eq!(format_name(b"RIFF...."), "wav");
        assert_eq!(format_name(b"\x01\x02"), "неизвестный контейнер");
    }

    #[test]
    fn postprocess_collapses_spaces_and_fixes_terms() {
        let mut cfg = crate::config::VoiceCfg::default();
        cfg.fix.insert("карго".into(), "cargo".into());
        let voice = Voice::new(&cfg);
        assert_eq!(voice.postprocess("  запусти   Карго  тест "), "запусти cargo тест");
    }

    /// Замены по умолчанию закрывают то, что видно в живом прогоне: модель
    /// пишет английские термины русскими словами.
    #[test]
    fn default_fixes_cover_typical_terms() {
        let voice = Voice::new(&crate::config::VoiceCfg::default());
        assert_eq!(
            voice.postprocess("запусти карго клиппи и открой мердж реквест в гитлаб"),
            "запусти cargo clippy и открой merge request в GitLab"
        );
    }

    /// Своя пара дополняет встроенные: дописав одну замену, человек не теряет
    /// остальные (раньше `[voice.fix]` в конфиге заменял список целиком).
    #[test]
    fn own_fixes_extend_defaults() {
        let mut cfg = crate::config::VoiceCfg::default();
        cfg.fix.clear();
        cfg.fix.insert("пин-код".into(), "PIN".into());
        let voice = Voice::new(&cfg);
        assert_eq!(voice.postprocess("карго и пин-код"), "cargo и PIN");
    }
}
