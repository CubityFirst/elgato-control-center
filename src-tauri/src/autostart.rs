//! "Start with Windows" through the per-user Run key (no admin rights needed).

#[cfg(windows)]
mod imp {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Where Settings > Apps > Startup records a switched-off entry, leaving the Run value in place.
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    /// Also referenced by the uninstaller hook in `windows/hooks.nsh`.
    const VALUE: &str = "Elgato Control Center";

    fn command() -> std::io::Result<String> {
        Ok(format!("\"{}\"", std::env::current_exe()?.display()))
    }

    fn registered() -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(RUN)
            .and_then(|k| k.get_value::<String, _>(VALUE))
            .ok()
    }

    fn switched_off() -> bool {
        // First byte is even when enabled (0x02) and odd when disabled (0x03).
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(APPROVED)
            .and_then(|k| k.get_raw_value(VALUE))
            .map(|v| v.bytes.first().is_some_and(|b| b & 1 == 1))
            .unwrap_or(false)
    }

    pub fn is_enabled() -> bool {
        registered().is_some() && !switched_off()
    }

    pub fn set_enabled(on: bool) -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if on {
            let (run, _) = hkcu.create_subkey(RUN)?;
            run.set_value(VALUE, &command()?)?;
            if let Ok(approved) = hkcu.open_subkey_with_flags(APPROVED, winreg::enums::KEY_SET_VALUE) {
                let _ = approved.delete_value(VALUE);
            }
        } else if let Ok(run) = hkcu.open_subkey_with_flags(RUN, winreg::enums::KEY_SET_VALUE) {
            match run.delete_value(VALUE) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Points an existing entry at this executable, in case the portable .exe was moved.
    pub fn sync_path() {
        if let (Some(current), Ok(wanted)) = (registered(), command()) {
            if !current.eq_ignore_ascii_case(&wanted) {
                let _ = RegKey::predef(HKEY_CURRENT_USER)
                    .open_subkey_with_flags(RUN, winreg::enums::KEY_SET_VALUE)
                    .and_then(|k| k.set_value(VALUE, &wanted));
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set_enabled(_on: bool) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub fn sync_path() {}
}

pub use imp::*;

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Touches the real HKCU Run key, restoring it afterwards: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn enable_disable_round_trip() {
        let was = is_enabled();
        set_enabled(true).unwrap();
        assert!(is_enabled());
        set_enabled(false).unwrap();
        assert!(!is_enabled());
        set_enabled(false).unwrap(); // already off is not an error
        set_enabled(was).unwrap();
    }
}
