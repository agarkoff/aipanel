//! Значок exe агента на Windows.
//!
//! Сам значок — `crates/agent/icon.ico`, его пересобирает `python assets/make-icons.py`.
//! Без Windows SDK (rc.exe) сборка не падает: значок нужен только проводнику
//! и диспетчеру задач, на работу агента он не влияет.

fn main() {
    println!("cargo:rerun-if-changed=icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon("icon.ico");
    res.set("ProductName", "AiPanel Agent");
    res.set("FileDescription", "AiPanel Agent");
    if let Err(err) = res.compile() {
        println!("cargo:warning=значок exe не приложен: {err}");
    }
}
