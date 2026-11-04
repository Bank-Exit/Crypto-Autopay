//! Windows Defender often flags the official monero-wallet-rpc (bundled as
//! `autopay-wallet-rpc.exe`) and quarantines it. With the user's consent (UAC prompt),
//! exclude that one file — not its folder — and bring it back from quarantine.

use std::path::{Path, PathBuf};

pub const SIDECAR: &str = "autopay-wallet-rpc.exe";

/// Where the installer puts the Monero engine (next to the app executable).
pub fn sidecar_path() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|d| d.join(SIDECAR))
}

/// PowerShell run as administrator. Exit 0: the file is back; 2: excluded but nothing to
/// restore (reinstall needed). Only the detection matching this exact path is restored.
pub fn allow_script(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\'', "''");
    [
        format!("$p = '{p}'"),
        "Add-MpPreference -ExclusionPath $p -ErrorAction Stop".into(),
        "if (-not (Test-Path -LiteralPath $p)) {".into(),
        "  $esc = [regex]::Escape($p)".into(),
        "  $names = Get-MpThreatDetection -ErrorAction SilentlyContinue |".into(),
        "    Where-Object { ($_.Resources -join ' ') -match $esc } |".into(),
        "    ForEach-Object { (Get-MpThreat -ThreatID $_.ThreatID -ErrorAction SilentlyContinue).ThreatName } |".into(),
        "    Sort-Object -Unique".into(),
        r#"  foreach ($n in $names) { & "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" -Restore -Name $n -FilePath $p | Out-Null }"#.into(),
        "}".into(),
        "if (Test-Path -LiteralPath $p) { exit 0 } else { exit 2 }".into(),
    ].join("\r\n")
}

/// Outcome of the elevated script.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Allow {
    Restored,
    ReinstallNeeded,
}

#[cfg(windows)]
pub fn allow() -> Result<Allow, String> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let path = sidecar_path().ok_or("App folder not found")?;
    let utf16: Vec<u8> = allow_script(&path).encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    // Non-elevated launcher: asks for elevation and hands back the elevated exit code.
    let launcher = format!(
        "try {{ $r = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $r.ExitCode }} \
         catch {{ exit 1223 }}"
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("PowerShell unavailable: {e}"))?;
    match status.code() {
        Some(0) => Ok(Allow::Restored),
        Some(2) => Ok(Allow::ReinstallNeeded),
        Some(1223) => Err("CANCELLED".into()),
        code => Err(format!("Defender exclusion failed (code {code:?})")),
    }
}

#[cfg(not(windows))]
pub fn allow() -> Result<Allow, String> {
    Err("Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_quotes_the_path() {
        let s = allow_script(Path::new(r"C:\Users\O'Brien\AppData\Local\Crypto AutoPay\autopay-wallet-rpc.exe"));
        assert!(s.contains(r"$p = 'C:\Users\O''Brien\AppData\Local\Crypto AutoPay\autopay-wallet-rpc.exe'"));
        assert!(s.contains("Add-MpPreference -ExclusionPath $p"));
        assert!(!s.contains("-Restore -All"));
    }
}
