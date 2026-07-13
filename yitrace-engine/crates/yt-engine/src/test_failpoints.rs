//! 只用于真实进程崩溃回归的停顿点。

use std::path::Path;

pub(crate) fn before_sidecar_rename(kind: &str, target: &Path) {
    #[cfg(feature = "test-failpoints")]
    {
        if std::env::var("YT_TEST_SIDECAR_BEFORE_RENAME")
            .ok()
            .as_deref()
            != Some(kind)
        {
            return;
        }
        let marker = std::env::var_os("YT_TEST_SIDECAR_MARKER")
            .map(std::path::PathBuf::from)
            .expect("YT_TEST_SIDECAR_MARKER is required when a sidecar failpoint is active");
        let message = format!("kind={kind}\ntarget={}\n", target.display());
        std::fs::write(&marker, message).expect("write sidecar failpoint marker");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    #[cfg(not(feature = "test-failpoints"))]
    let _ = (kind, target);
}
