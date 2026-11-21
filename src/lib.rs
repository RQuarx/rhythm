use thiserror::Error;

mod app;
mod settings;
mod user;

slint::include_modules!();

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    PlatformError(#[from] slint::PlatformError),

    #[error("{0}")]
    ApplicationError(#[from] app::Error),
}

pub fn run() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = app::Application::new()?;
    let ui = MainWindow::new()?;

    ui.on_page_changed(|index| println!("{index}"));
    ui.run().map_err(|e| Error::PlatformError(e))
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    slint::android::init(app).unwrap();
    run().unwrap();
}
