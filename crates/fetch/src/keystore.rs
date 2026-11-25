//! Where the JustTCG key is kept: Windows Credential Manager on Windows, elsewhere a file in the
//! card-ledger data folder (`justtcg.key`, never inside a repo). `card-ledger justtcg-key set`
//! is the only way to save one; the window can't.
//!
//! The key is never printed, logged or put in an error. Errors from a store name the store and
//! what failed, never the value. Outside this module the key is shown only as [`mask`] gives it:
//! its last four characters.

use std::fmt;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The Credential Manager entry: service "card-ledger", user "justtcg" (target
/// `justtcg.card-ledger`).
pub const SERVICE: &str = "card-ledger";
pub const USER: &str = "justtcg";
/// The file store's name inside the data folder.
pub const FILE_NAME: &str = "justtcg.key";
/// Shortest and longest key `set` accepts.
pub const MIN_LEN: usize = 16;
pub const MAX_LEN: usize = 200;
/// Keys shorter than this (only possible through the environment variable) show no ending.
const MASK_MIN_LEN: usize = 12;

/// A place the key can be kept. Tests use [`MemoryStore`]; nothing in a test touches the real
/// Credential Manager or the real data folder.
pub trait KeyStore: Send + Sync {
    /// "Windows Credential Manager (justtcg.card-ledger)" or "file C:\…\justtcg.key".
    fn location(&self) -> String;
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, key: &str) -> Result<(), String>;
    /// True when there was a key to remove.
    fn clear(&self) -> Result<bool, String>;
}

/// The store this computer uses: Credential Manager on Windows, the data-folder file elsewhere.
pub fn default_store(data_dir: &Path) -> Box<dyn KeyStore> {
    #[cfg(windows)]
    {
        let _ = data_dir;
        Box::new(CredentialManager)
    }
    #[cfg(not(windows))]
    {
        Box::new(FileStore::new(data_dir))
    }
}

/// Windows Credential Manager, through the `keyring` crate.
#[cfg(windows)]
pub struct CredentialManager;

#[cfg(windows)]
impl CredentialManager {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, USER).map_err(|e| cm_error("open", &e))
    }
}

/// A Credential Manager error, without anything the credential held (`BadEncoding` carries the
/// stored bytes, so it gets a fixed message).
#[cfg(windows)]
fn cm_error(what: &str, e: &keyring::Error) -> String {
    let why = match e {
        keyring::Error::BadEncoding(_) => "the stored entry isn't text".to_string(),
        keyring::Error::PlatformFailure(p) => p.to_string(),
        keyring::Error::NoStorageAccess(p) => format!("no access ({p})"),
        other => other.to_string(),
    };
    format!("Windows Credential Manager: can't {what} the JustTCG key: {why}")
}

#[cfg(windows)]
impl KeyStore for CredentialManager {
    fn location(&self) -> String {
        format!("Windows Credential Manager ({USER}.{SERVICE})")
    }
    fn get(&self) -> Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(cm_error("read", &e)),
        }
    }
    fn set(&self, key: &str) -> Result<(), String> {
        Self::entry()?
            .set_password(key)
            .map_err(|e| cm_error("save", &e))
    }
    fn clear(&self) -> Result<bool, String> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(cm_error("remove", &e)),
        }
    }
}

/// A file in the data folder holding just the key. On Unix only its owner can read it.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(data_dir: &Path) -> FileStore {
        FileStore {
            path: data_dir.join(FILE_NAME),
        }
    }
}

impl KeyStore for FileStore {
    fn location(&self) -> String {
        format!("file {}", self.path.display())
    }
    fn get(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("can't read {}: {e}", self.path.display())),
        }
    }
    fn set(&self, key: &str) -> Result<(), String> {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&self.path)
            .map_err(|e| format!("can't write {}: {e}", self.path.display()))?;
        // `mode` only applies when the file is created; tighten an existing file too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("can't restrict {}: {e}", self.path.display()))?;
        }
        f.write_all(key.as_bytes())
            .map_err(|e| format!("can't write {}: {e}", self.path.display()))
    }
    fn clear(&self) -> Result<bool, String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("can't remove {}: {e}", self.path.display())),
        }
    }
}

/// A store in memory, for tests.
#[derive(Default)]
pub struct MemoryStore {
    key: Mutex<Option<String>>,
}

impl MemoryStore {
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }
    pub fn holding(key: &str) -> MemoryStore {
        MemoryStore {
            key: Mutex::new(Some(key.to_string())),
        }
    }
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.key.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MemoryStore({})",
            if self.slot().is_some() {
                "set"
            } else {
                "empty"
            }
        )
    }
}

impl KeyStore for MemoryStore {
    fn location(&self) -> String {
        "memory (test)".into()
    }
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.slot().clone())
    }
    fn set(&self, key: &str) -> Result<(), String> {
        *self.slot() = Some(key.to_string());
        Ok(())
    }
    fn clear(&self) -> Result<bool, String> {
        Ok(self.slot().take().is_some())
    }
}

/// The key's last four characters, or `None` for a key too short to show any of.
pub fn mask(key: &str) -> Option<String> {
    let chars: Vec<char> = key.trim().chars().collect();
    (chars.len() >= MASK_MIN_LEN).then(|| chars[chars.len() - 4..].iter().collect())
}

/// Checks a pasted key: one token of 16 to 200 printable ASCII characters. The error never
/// repeats the key.
pub fn check_format(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("no key given".into());
    }
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("a key is letters, digits and punctuation only, with no spaces".into());
    }
    if key.len() < MIN_LEN || key.len() > MAX_LEN {
        return Err(format!(
            "that is {} characters; a JustTCG key is {MIN_LEN} to {MAX_LEN}",
            key.len()
        ));
    }
    Ok(())
}

/// Reads one line from `input` and trims it. Nothing is printed.
pub fn read_key_line(input: &mut dyn BufRead) -> Result<String, String> {
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|e| format!("can't read the key from standard input: {e}"))?;
    let key = line.trim().to_string();
    Ok(key)
}

/// Reads the key from standard input. When standard input is a Windows console, typing isn't
/// shown while it is read.
pub fn read_key_from_stdin() -> Result<String, String> {
    // Echo comes back when the guard goes out of scope at the end of this block.
    let _quiet = console::EchoOff::start();
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    read_key_line(&mut lock)
}

#[cfg(windows)]
mod console {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_ECHO_INPUT,
        STD_INPUT_HANDLE,
    };

    /// Turns console echo off until dropped. Does nothing when standard input isn't a console.
    pub struct EchoOff(Option<CONSOLE_MODE>);

    impl EchoOff {
        pub fn start() -> EchoOff {
            // SAFETY: plain Win32 console calls on this process's own standard input handle.
            unsafe {
                let h = GetStdHandle(STD_INPUT_HANDLE);
                let mut mode: CONSOLE_MODE = 0;
                if GetConsoleMode(h, &mut mode) == 0
                    || SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT) == 0
                {
                    return EchoOff(None);
                }
                EchoOff(Some(mode))
            }
        }
    }

    impl Drop for EchoOff {
        fn drop(&mut self) {
            if let Some(mode) = self.0 {
                // SAFETY: as above, restoring the mode read in `start`.
                unsafe {
                    SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode);
                }
                // The Enter key wasn't echoed either.
                eprintln!();
            }
        }
    }
}

#[cfg(not(windows))]
mod console {
    pub struct EchoOff;
    impl EchoOff {
        pub fn start() -> EchoOff {
            EchoOff
        }
    }
}
