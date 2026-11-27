//! Clipboard access through a native library.
//!
//! Every operation goes through `arboard`, which reaches the compositor's clipboard
//! over the Wayland data-control protocol when the session offers it and falls back to
//! X11. No helper program is spawned, so there is no child process to wait for, reap or
//! repair a `TMPDIR` for: what used to be subprocess plumbing is a library call.
//!
//! The library calls still block until the selection owner answers, so the application
//! never makes them from the event loop. Writes go through [`ClipboardWriter`], which
//! runs them on one background thread and keeps one `arboard` handle alive there, and
//! reads run on the thread `Application::paste` spawns.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use arboard::{
    Clipboard as NativeClipboard, Error as NativeError, GetExtLinux, LinuxClipboardKind,
    SetExtLinux,
};

/// Largest clipboard image a paste accepts, in bytes of encoded PNG.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Largest clipboard image a paste accepts, in pixels.
///
/// Four bytes per pixel are held in memory while the image is encoded, so this keeps a
/// clipboard payload from turning into an unbounded allocation.
pub const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

/// Where a pasted image goes when `TMPDIR` no longer names a directory.
const FALLBACK_TEMP_DIR: &str = "/tmp";

/// Reported when the session offers no clipboard at all.
const UNAVAILABLE: &str = "clipboard is unavailable (the session offers no clipboard)";

/// Reported when the clipboard holds neither text nor an image.
const EMPTY: &str = "the clipboard holds no text or image";

/// Reported when the clipboard holds something that is not a decodable image.
const UNDECODABLE: &str = "clipboard image could not be decoded";

/// Reported when the clipboard worker thread never started, which is the only reason a
/// queued write can be refused.
const WORKER_UNAVAILABLE: &str = "the clipboard worker is not running";

/// How many names one paste tries before giving up on a free one.
const NAME_ATTEMPTS: u32 = 8;

/// Which selection to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// `Ctrl+C`/`Ctrl+V` clipboard.
    Clipboard,
    /// X11/Wayland primary selection (copy-on-select target).
    Primary,
}

impl Selection {
    fn kind(self) -> LinuxClipboardKind {
        match self {
            Self::Clipboard => LinuxClipboardKind::Clipboard,
            Self::Primary => LinuxClipboardKind::Primary,
        }
    }
}

/// Clipboard backend state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `arboard` opened the session's clipboard.
    Native,
    /// No clipboard backend: copy and paste are refused with a message.
    Unavailable,
}

/// A clipboard read that passed the trust checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paste {
    /// Plain text, exactly as the clipboard holds it.
    Text(String),
    /// An image, already written to `path` inside the temp directory.
    Image {
        /// File the PNG was written to.
        path: PathBuf,
    },
}

/// What a backend handed back before the payload was checked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Raw {
    /// Text, still untrimmed and unnormalised.
    Text(String),
    /// PNG bytes, not yet trusted to be a PNG.
    Image(Vec<u8>),
}

/// Performs one clipboard read.
///
/// Injected so no test needs a real clipboard, exactly like the writer's runner.
type Reader = dyn Fn(Selection) -> Result<Raw, String> + Send + Sync + 'static;

/// Clipboard service.
#[derive(Clone)]
pub struct Clipboard {
    backend: Backend,
    reader: Arc<Reader>,
}

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Clipboard")
            .field("backend", &self.backend)
            .finish()
    }
}

impl Clipboard {
    /// Detects whether the session offers a clipboard.
    ///
    /// The probe opens and drops one handle, which is what turns "no display, no
    /// compositor, no data-control protocol" into a service that reports failure
    /// instead of one that panics on the first paste.
    pub fn detect() -> Self {
        match NativeClipboard::new() {
            Ok(probe) => {
                drop(probe);
                Self {
                    backend: Backend::Native,
                    reader: Arc::new(native_read),
                }
            }
            Err(error) => {
                tracing::debug!(error = %error, "no native clipboard backend");
                Self {
                    backend: Backend::Unavailable,
                    reader: Arc::new(|_selection| Err(UNAVAILABLE.to_owned())),
                }
            }
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn is_available(&self) -> bool {
        self.backend == Backend::Native
    }

    /// A writer that runs the clipboard writes on a background thread.
    ///
    /// This is the path for anything the UI thread touches: opening the handle, setting
    /// the selection and any wait inside the library all happen on the worker, so a
    /// compositor that stalls cannot freeze a frame. With no backend at all the writer
    /// refuses every copy with the same message a read would report.
    pub fn writer(&self) -> ClipboardWriter {
        if !self.is_available() {
            return ClipboardWriter::refusing(UNAVAILABLE.to_owned());
        }
        ClipboardWriter::start(native_writer())
    }

    /// Reads the clipboard, off the calling thread.
    ///
    /// This waits for the selection owner to answer, so the event loop never calls it —
    /// see `Application::paste`. Text wins over an image: the image is only materialised
    /// when the clipboard offers no text at all.
    ///
    /// `Err` carries a message for the status bar: no backend, an empty clipboard, or a
    /// payload that failed the limits below.
    pub fn read(&self, selection: Selection) -> Result<Paste, String> {
        self.read_into(selection, &image_dir())
    }

    /// A read that writes any image into `dir` instead of the detected temp directory.
    fn read_into(&self, selection: Selection, dir: &Path) -> Result<Paste, String> {
        match (self.reader)(selection)? {
            Raw::Text(text) => Ok(Paste::Text(text)),
            Raw::Image(png) => write_image(&png, dir),
        }
    }
}

/// The two lookups one read makes, in the order it makes them.
///
/// A trait rather than two closures so the order, the text-first rule and the failure
/// messages are the same code in production and in tests.
trait Offered {
    fn text(&mut self) -> Result<String, NativeError>;
    fn image(&mut self) -> Result<(usize, usize, Vec<u8>), NativeError>;
}

/// The real clipboard, for one selection.
struct NativeSource {
    clipboard: NativeClipboard,
    kind: LinuxClipboardKind,
}

impl NativeSource {
    fn open(selection: Selection) -> Result<Self, String> {
        Ok(Self {
            clipboard: NativeClipboard::new().map_err(describe_error)?,
            kind: selection.kind(),
        })
    }
}

impl Offered for NativeSource {
    fn text(&mut self) -> Result<String, NativeError> {
        self.clipboard.get().clipboard(self.kind).text()
    }

    fn image(&mut self) -> Result<(usize, usize, Vec<u8>), NativeError> {
        self.clipboard
            .get()
            .clipboard(self.kind)
            .image()
            .map(|image| (image.width, image.height, image.bytes.into_owned()))
    }
}

/// One native read: text first, the image only when the clipboard offers no text.
fn native_read(selection: Selection) -> Result<Raw, String> {
    let mut source = NativeSource::open(selection)?;
    read_from(&mut source)
}

/// Reads whichever format the source offers, deciding text priority.
fn read_from(source: &mut impl Offered) -> Result<Raw, String> {
    match source.text() {
        Ok(text) => return Ok(Raw::Text(text)),
        // Not an error: an image on the clipboard simply does not offer text.
        Err(NativeError::ContentNotAvailable) => {}
        Err(error) => return Err(describe_error(error)),
    }
    match source.image() {
        Ok((width, height, rgba)) => Ok(Raw::Image(encode_png(width, height, &rgba)?)),
        Err(NativeError::ContentNotAvailable) => Err(EMPTY.to_owned()),
        Err(error) => Err(describe_error(error)),
    }
}

/// Encodes clipboard pixels as PNG, refusing an image too large to be a paste.
fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let pixels = u64::try_from(width)
        .ok()
        .zip(u64::try_from(height).ok())
        .and_then(|(width, height)| width.checked_mul(height));
    let Some(pixels) = pixels.filter(|pixels| *pixels > 0) else {
        return Err(UNDECODABLE.to_owned());
    };
    if pixels > MAX_IMAGE_PIXELS {
        return Err(format!(
            "clipboard image is larger than {} megapixels; copy a smaller image",
            MAX_IMAGE_PIXELS / (1024 * 1024)
        ));
    }
    // A buffer that cannot hold the pixels it claims is not an image.
    if rgba.len() < width.saturating_mul(height).saturating_mul(4) {
        return Err(UNDECODABLE.to_owned());
    }
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(UNDECODABLE.to_owned());
    };

    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| UNDECODABLE.to_owned())?;
        writer
            .write_image_data(rgba)
            .map_err(|_| UNDECODABLE.to_owned())?;
    }
    Ok(png)
}

/// Checks an untrusted image payload and writes it into `dir`.
///
/// Nothing is written unless the payload is small enough and decodes, and the only
/// directory ever written to is the one handed in by [`image_dir`].
fn write_image(png: &[u8], dir: &Path) -> Result<Paste, String> {
    if png.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "clipboard image is larger than {} MiB; copy a smaller image",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    validate_png(png)?;
    let path = create_image_file(png, dir)?;
    Ok(Paste::Image { path })
}

/// Accepts only a PNG the decoder can read, and only within the pixel limit.
fn validate_png(png: &[u8]) -> Result<(), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_limits(png::Limits {
        bytes: MAX_IMAGE_BYTES,
    });
    let reader = decoder.read_info().map_err(|_| UNDECODABLE.to_owned())?;
    let info = reader.info();
    if u64::from(info.width) * u64::from(info.height) > MAX_IMAGE_PIXELS {
        return Err(format!(
            "clipboard image is larger than {} megapixels; copy a smaller image",
            MAX_IMAGE_PIXELS / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Writes `png` under a name no other paste can be holding.
///
/// The name is built only from this process id, a counter and a timestamp, so no
/// clipboard content ever reaches the filesystem, and the file is created with
/// `create_new`, which refuses to follow a name that already exists — a paste cannot be
/// talked into overwriting or following a symlink in a shared temp directory.
fn create_image_file(png: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let path = dir.join(next_image_name());
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(png) {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("could not write the pasted image: {error}"));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt >= NAME_ATTEMPTS {
                    return Err("could not find a free name for the pasted image".to_owned());
                }
            }
            Err(error) => return Err(format!("could not write the pasted image: {error}")),
        }
    }
}

/// Names one pasted image file.
fn next_image_name() -> String {
    let serial = IMAGE_SERIAL.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "astraterm-paste-{}-{serial}-{nanos}.png",
        std::process::id()
    )
}

/// Distinguishes the pastes of one process.
static IMAGE_SERIAL: AtomicU64 = AtomicU64::new(0);

/// The directory a pasted image is written to.
///
/// `TMPDIR` is used while it still names a directory. A terminal can outlive the session
/// temp directory it started with — the failure that used to make `wl-copy` report
/// `mkdtemp: No such file or directory` — so a stale `TMPDIR` falls back to `/tmp`
/// instead of failing every paste.
pub fn image_dir() -> PathBuf {
    image_dir_with(std::env::var_os("TMPDIR").map(PathBuf::from), |path| {
        path.is_dir()
    })
}

/// The decision, with the filesystem check injected so it can be tested.
fn image_dir_with(raw: Option<PathBuf>, is_dir: impl Fn(&Path) -> bool) -> PathBuf {
    raw.filter(|dir| is_dir(dir))
        .unwrap_or_else(|| PathBuf::from(FALLBACK_TEMP_DIR))
}

/// The write side of the native clipboard: one handle, kept alive on the worker thread.
///
/// The handle has to outlive the call that sets the selection, because on X11 it owns
/// the connection that answers other applications' requests: dropping it right after a
/// copy would take the selection with it.
fn native_writer() -> Arc<Runner> {
    let handle = Mutex::new(None::<NativeClipboard>);
    Arc::new(move |selection: Selection, text: &str| {
        let mut handle = lock(&handle);
        if handle.is_none() {
            *handle = Some(NativeClipboard::new().map_err(describe_error)?);
        }
        let Some(clipboard) = handle.as_mut() else {
            return Err(WORKER_UNAVAILABLE.to_owned());
        };
        clipboard
            .set()
            .clipboard(selection.kind())
            .text(text)
            .map_err(describe_error)
    })
}

/// A library failure, phrased for the status bar.
fn describe_error(error: NativeError) -> String {
    match error {
        NativeError::ContentNotAvailable => EMPTY.to_owned(),
        NativeError::ClipboardNotSupported => UNAVAILABLE.to_owned(),
        NativeError::ClipboardOccupied => "the clipboard is held by another application".to_owned(),
        NativeError::ConversionFailure => UNDECODABLE.to_owned(),
        other => format!("clipboard: {other}"),
    }
}

/// A copy queued for the worker thread.
struct Job {
    selection: Selection,
    text: String,
    done: Option<Done>,
}

/// Called once with the outcome of the write that ran the copy.
type Done = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

/// Performs one clipboard write.
///
/// Injected so no test needs a real clipboard and so a stalled write can be simulated
/// exactly. Public because the application's own copy paths are tested from its crate,
/// through this seam and not through the compositor.
pub type Runner = dyn Fn(Selection, &str) -> Result<(), String> + Send + Sync + 'static;

/// State shared with the worker thread.
struct Shared {
    /// The newest write that has not started yet. One slot, not a queue: a burst of
    /// selections replaces the pending write instead of building a backlog.
    pending: Mutex<Option<Job>>,
    ready: Condvar,
    runner: Arc<Runner>,
    /// Set when this writer can never run a write; every copy is refused with it.
    refusal: Option<String>,
}

/// Writes to the clipboard from a single background thread.
///
/// Cloning is cheap: every clone shares the one worker and its one pending slot.
#[derive(Clone)]
pub struct ClipboardWriter {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ClipboardWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ClipboardWriter").finish()
    }
}

impl ClipboardWriter {
    /// A writer that runs `runner` for every queued write.
    ///
    /// [`Clipboard::writer`] is what production uses; this is the seam that lets a test
    /// in another crate stand in for the native clipboard, so the copy paths are asserted
    /// without a compositor and without timing.
    pub fn start(runner: Arc<Runner>) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(None),
            ready: Condvar::new(),
            runner,
            refusal: None,
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("astraterm-clipboard".to_owned())
            .spawn(move || work(&worker));
        if spawned.is_err() {
            return Self::refusing(WORKER_UNAVAILABLE.to_owned());
        }
        Self { shared }
    }

    /// A writer that starts no worker: every copy is refused with `refusal`.
    ///
    /// This is how a machine without any clipboard backend keeps reporting that instead
    /// of silently accepting copies nobody can read.
    fn refusing(refusal: String) -> Self {
        let never_used = refusal.clone();
        Self {
            shared: Arc::new(Shared {
                pending: Mutex::new(None),
                ready: Condvar::new(),
                // No copy ever reaches this runner: the refusal is returned first.
                runner: Arc::new(move |_: Selection, _: &str| Err(never_used.clone()))
                    as Arc<Runner>,
                refusal: Some(refusal),
            }),
        }
    }

    /// Queues `text` and returns without waiting for the write.
    ///
    /// A write that has not started yet is replaced, so two selections in quick
    /// succession leave the clipboard holding the second one.
    pub fn set(&self, selection: Selection, text: &str) -> Result<(), String> {
        self.set_reported(selection, text, |_| {})
    }

    /// Queues `text` and reports the outcome to `done` from the worker.
    ///
    /// A queued write that a newer one replaces never reports: it never ran.
    pub fn set_reported(
        &self,
        selection: Selection,
        text: &str,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), String> {
        if let Some(refusal) = &self.shared.refusal {
            return Err(refusal.clone());
        }
        {
            let mut slot = lock(&self.shared.pending);
            *slot = Some(Job {
                selection,
                text: text.to_owned(),
                done: Some(Box::new(done)),
            });
        }
        self.shared.ready.notify_one();
        Ok(())
    }
}

/// The worker loop: take the newest queued write, run it, report it, repeat.
fn work(shared: &Shared) {
    loop {
        let job = next_job(shared);
        let result = (shared.runner)(job.selection, &job.text);
        if let Some(done) = job.done {
            done(result);
        }
    }
}

/// Waits for a queued write. Never exits, so the worker outlives every copy.
fn next_job(shared: &Shared) -> Job {
    let mut slot = lock(&shared.pending);
    loop {
        if let Some(job) = slot.take() {
            return job;
        }
        slot = shared
            .ready
            .wait(slot)
            .unwrap_or_else(|error| error.into_inner());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Duration;

    use super::*;

    /// A rendezvous that lets a test hold a write open for as long as it needs, so "the
    /// caller did not wait" is proven by control flow and never by timing.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = lock(&self.open);
            while !*open {
                open = self
                    .changed
                    .wait(open)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }

        fn open(&self) {
            *lock(&self.open) = true;
            self.changed.notify_all();
        }
    }

    /// A fake clipboard write path: records every copy it is asked to run.
    #[derive(Clone, Default)]
    struct Fake {
        started: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn started(&self) -> Vec<String> {
            lock(&self.started).clone()
        }

        /// A runner that records the text and then blocks on `gate`.
        fn gated(&self, gate: Arc<Gate>, signal: mpsc::Sender<String>) -> Arc<Runner> {
            let started = Arc::clone(&self.started);
            Arc::new(move |_selection, text: &str| {
                lock(&started).push(text.to_owned());
                let _ = signal.send(text.to_owned());
                gate.wait();
                Ok(())
            })
        }
    }

    /// A fake backend: what the clipboard currently holds, shared by a fake writer and a
    /// fake reader, so a copy and the paste after it meet.
    #[derive(Clone, Default)]
    struct Store {
        text: Arc<Mutex<Option<String>>>,
    }

    impl Store {
        fn runner(&self) -> Arc<Runner> {
            let text = Arc::clone(&self.text);
            Arc::new(move |_selection, written: &str| {
                *lock(&text) = Some(written.to_owned());
                Ok(())
            })
        }

        fn reader(&self) -> Arc<Reader> {
            let text = Arc::clone(&self.text);
            Arc::new(move |_selection| match lock(&text).clone() {
                Some(text) => Ok(Raw::Text(text)),
                None => Err(EMPTY.to_owned()),
            })
        }

        fn clipboard(&self) -> Clipboard {
            Clipboard {
                backend: Backend::Native,
                reader: self.reader(),
            }
        }
    }

    /// A source with both formats on offer, counting the image lookups.
    struct FakeSource {
        text: Option<&'static str>,
        image: Option<(usize, usize, Vec<u8>)>,
        image_reads: Cell<u32>,
    }

    impl FakeSource {
        fn image_only(pixels: (usize, usize, Vec<u8>)) -> Self {
            Self {
                text: None,
                image: Some(pixels),
                image_reads: Cell::new(0),
            }
        }
    }

    impl Offered for FakeSource {
        fn text(&mut self) -> Result<String, NativeError> {
            self.text
                .map(str::to_owned)
                .ok_or(NativeError::ContentNotAvailable)
        }

        fn image(&mut self) -> Result<(usize, usize, Vec<u8>), NativeError> {
            self.image_reads.set(self.image_reads.get() + 1);
            self.image.clone().ok_or(NativeError::ContentNotAvailable)
        }
    }

    fn receive<T>(receiver: &mpsc::Receiver<T>, what: &str) -> T {
        receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| panic!("the worker never reported {what}: {error}"))
    }

    /// A private directory for one test, removed when the test ends.
    ///
    /// It lives in the system temp directory, never in the worktree, and the name is
    /// fresh every run so a leftover from an interrupted run cannot skew a count.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let path = std::env::temp_dir().join(format!(
                "astraterm-clipboard-test-{}-{name}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("the scratch directory");
            Self { path }
        }

        fn entries(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.path)
                .expect("the scratch directory is readable")
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).ok();
        }
    }

    /// A tiny valid PNG, encoded by the same code the native read path uses.
    fn tiny_png() -> Vec<u8> {
        let rgba: Vec<u8> = (0..16).collect();
        encode_png(2, 2, &rgba).expect("a two by two image encodes")
    }

    fn image_reader(png: Vec<u8>) -> Arc<Reader> {
        Arc::new(move |_selection| Ok(Raw::Image(png.clone())))
    }

    #[test]
    fn detection_never_panics() {
        let clipboard = Clipboard::detect();
        assert_eq!(
            clipboard.is_available(),
            clipboard.backend() == Backend::Native
        );
    }

    #[test]
    fn an_unavailable_backend_reports_failure_instead_of_panicking() {
        let clipboard = Clipboard {
            backend: Backend::Unavailable,
            reader: Arc::new(|_selection| Err(UNAVAILABLE.to_owned())),
        };
        assert_eq!(
            clipboard.read(Selection::Clipboard).unwrap_err(),
            UNAVAILABLE
        );
        assert_eq!(
            clipboard
                .writer()
                .set(Selection::Clipboard, "text")
                .unwrap_err(),
            UNAVAILABLE
        );
    }

    #[test]
    fn text_round_trips_unchanged() {
        // S5: whatever the copy wrote is what the paste sends, byte for byte — including
        // the leading and trailing spaces the old `wl-paste --no-newline` preserved.
        let store = Store::default();
        let writer = ClipboardWriter::start(store.runner());
        let text = " hola\rmundo \n";

        let (report, reported) = mpsc::channel();
        assert!(
            writer
                .set_reported(Selection::Clipboard, text, move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert!(receive(&reported, "the copy").is_ok());

        assert_eq!(
            store
                .clipboard()
                .read_into(Selection::Clipboard, &std::env::temp_dir())
                .unwrap(),
            Paste::Text(text.to_owned())
        );
    }

    #[test]
    fn an_empty_clipboard_reports_a_message() {
        let store = Store::default();
        assert_eq!(
            store
                .clipboard()
                .read_into(Selection::Clipboard, &std::env::temp_dir())
                .unwrap_err(),
            EMPTY
        );
    }

    #[test]
    fn text_wins_over_an_image_when_the_clipboard_offers_both() {
        let mut source = FakeSource {
            text: Some("hola"),
            image: Some((1, 1, vec![0, 0, 0, 255])),
            image_reads: Cell::new(0),
        };
        assert_eq!(
            read_from(&mut source).unwrap(),
            Raw::Text("hola".to_owned())
        );
        assert_eq!(
            source.image_reads.get(),
            0,
            "the image is never consulted while text is on offer"
        );
    }

    #[test]
    fn an_image_is_read_only_when_there_is_no_text() {
        let png = tiny_png();
        let mut source = FakeSource::image_only((2, 2, (0..16).collect()));
        assert_eq!(read_from(&mut source).unwrap(), Raw::Image(png));
        assert_eq!(source.image_reads.get(), 1);
    }

    #[test]
    fn a_clipboard_image_becomes_a_png_file_under_the_temp_directory() {
        let scratch = Scratch::new("image");
        let png = tiny_png();
        let clipboard = Clipboard {
            backend: Backend::Native,
            reader: image_reader(png.clone()),
        };

        let Paste::Image { path } = clipboard
            .read_into(Selection::Clipboard, &scratch.path)
            .expect("the image is pasted as a path")
        else {
            panic!("an image on the clipboard is pasted as a file path");
        };

        assert!(
            path.starts_with(&scratch.path),
            "{path:?} is under the temp directory"
        );
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).unwrap(), png, "the file holds the PNG");
    }

    #[test]
    fn two_pastes_never_share_a_file_name() {
        let scratch = Scratch::new("unique");
        let png = tiny_png();
        let clipboard = Clipboard {
            backend: Backend::Native,
            reader: image_reader(png.clone()),
        };

        let first = clipboard
            .read_into(Selection::Clipboard, &scratch.path)
            .unwrap();
        let second = clipboard
            .read_into(Selection::Clipboard, &scratch.path)
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(scratch.entries().len(), 2, "both pastes are on disk");
    }

    #[test]
    fn an_oversized_image_is_refused_and_writes_nothing() {
        let scratch = Scratch::new("oversized");
        let oversized = vec![0_u8; MAX_IMAGE_BYTES + 1];
        let clipboard = Clipboard {
            backend: Backend::Native,
            reader: image_reader(oversized),
        };

        let error = clipboard
            .read_into(Selection::Clipboard, &scratch.path)
            .unwrap_err();
        assert!(error.contains("larger than"), "{error}");
        assert!(scratch.entries().is_empty(), "nothing was written");
    }

    #[test]
    fn an_undecodable_payload_is_refused_and_writes_nothing() {
        let scratch = Scratch::new("undecodable");
        let clipboard = Clipboard {
            backend: Backend::Native,
            reader: image_reader(b"wl-paste returned bytes".to_vec()),
        };

        assert_eq!(
            clipboard
                .read_into(Selection::Clipboard, &scratch.path)
                .unwrap_err(),
            UNDECODABLE
        );
        assert!(scratch.entries().is_empty(), "nothing was written");
    }

    #[test]
    fn an_image_beyond_the_pixel_limit_is_refused_before_encoding() {
        // Checked from the dimensions alone, so the refusal never allocates the buffer a
        // hostile clipboard claims to hold.
        let error = encode_png(100_000, 100_000, &[]).unwrap_err();
        assert!(error.contains("megapixels"), "{error}");
    }

    #[test]
    fn a_png_with_a_truncated_pixel_buffer_is_refused() {
        assert_eq!(encode_png(4, 4, &[0, 0, 0, 255]).unwrap_err(), UNDECODABLE);
    }

    #[test]
    fn a_tmpdir_that_no_longer_exists_falls_back_instead_of_failing_the_paste() {
        // A terminal outliving its session temp directory used to fail every copy; the
        // image paste must not inherit that failure.
        let missing = PathBuf::from("/tmp/gone-with-the-session");
        assert_eq!(
            image_dir_with(Some(missing), |_| false),
            PathBuf::from(FALLBACK_TEMP_DIR)
        );
        assert_eq!(
            image_dir_with(None, |_| true),
            PathBuf::from(FALLBACK_TEMP_DIR)
        );

        let present = PathBuf::from("/tmp");
        assert_eq!(
            image_dir_with(Some(present.clone()), |_| true),
            present,
            "a real directory is used as it is"
        );
    }

    #[test]
    fn an_empty_primary_selection_reports_a_message_instead_of_panicking() {
        // Middle-click paste reads the primary selection. With nothing selected it holds
        // no text and no image, so the read fails with the same message an empty clipboard
        // reports and the paste sends nothing; the regular clipboard is untouched.
        let clipboard_text = Arc::new(Mutex::new(Some("clipboard text".to_owned())));
        let text = Arc::clone(&clipboard_text);
        let reader: Arc<Reader> = Arc::new(move |selection| match selection {
            Selection::Clipboard => match lock(&text).clone() {
                Some(text) => Ok(Raw::Text(text)),
                None => Err(EMPTY.to_owned()),
            },
            // An empty primary selection offers neither format.
            Selection::Primary => Err(EMPTY.to_owned()),
        });
        let clipboard = Clipboard {
            backend: Backend::Native,
            reader,
        };

        assert_eq!(clipboard.read(Selection::Primary).unwrap_err(), EMPTY);
        assert_eq!(
            clipboard.read(Selection::Clipboard).unwrap(),
            Paste::Text("clipboard text".to_owned()),
            "the empty primary selection leaves the clipboard alone"
        );
    }

    #[test]
    fn a_primary_write_reaches_the_runner_as_the_primary_selection() {
        // The write side uses one backend for both selections: the job carries the
        // selection through to the runner instead of hard-coding the clipboard.
        let seen = Arc::new(Mutex::new(Vec::<(Selection, String)>::new()));
        let recorded = Arc::clone(&seen);
        let writer = ClipboardWriter::start(Arc::new(move |selection, text: &str| {
            lock(&recorded).push((selection, text.to_owned()));
            Ok(())
        }));
        let (report, reported) = mpsc::channel();

        assert!(
            writer
                .set_reported(Selection::Primary, "selected", move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert!(receive(&reported, "the primary write").is_ok());
        assert_eq!(
            *lock(&seen),
            vec![(Selection::Primary, "selected".to_owned())]
        );
    }

    #[test]
    fn a_copy_returns_before_the_write_finishes() {
        // The reported freeze: the write never returns and the caller must get past both
        // enqueues while it is still blocked, which the gate proves without timing.
        let fake = Fake::default();
        let gate = Arc::new(Gate::default());
        let (signal, signalled) = mpsc::channel();
        let writer = ClipboardWriter::start(fake.gated(Arc::clone(&gate), signal));

        assert!(writer.set(Selection::Clipboard, "first").is_ok());
        assert_eq!(receive(&signalled, "the first copy"), "first");
        // The write is still running here, and this returns regardless.
        assert!(writer.set(Selection::Clipboard, "second").is_ok());
        assert_eq!(fake.started(), vec!["first".to_owned()]);

        gate.open();
        assert_eq!(receive(&signalled, "the second copy"), "second");
        assert_eq!(
            fake.started(),
            vec!["first".to_owned(), "second".to_owned()],
            "the write that had to wait still ran, after the one in flight"
        );
    }

    #[test]
    fn a_newer_copy_replaces_the_pending_one_instead_of_queueing_it() {
        // A drag can report several selections while one write is running. The slot holds
        // only the newest, so the clipboard ends up holding that one and no backlog of
        // writes is ever started.
        let fake = Fake::default();
        let gate = Arc::new(Gate::default());
        let (signal, signalled) = mpsc::channel();
        let writer = ClipboardWriter::start(fake.gated(Arc::clone(&gate), signal));

        assert!(writer.set(Selection::Clipboard, "first").is_ok());
        assert_eq!(receive(&signalled, "the first copy"), "first");
        assert!(writer.set(Selection::Clipboard, "superseded").is_ok());
        assert!(writer.set(Selection::Clipboard, "last").is_ok());

        gate.open();
        assert_eq!(receive(&signalled, "the last copy"), "last");
        assert_eq!(
            fake.started(),
            vec!["first".to_owned(), "last".to_owned()],
            "the superseded copy never started"
        );
    }

    #[test]
    fn an_explicit_copy_is_never_deduped() {
        // `ctrl+shift+c` twice on the same selection must copy twice: the "unchanged
        // selection" rule belongs to the copy-on-select gesture, not to the write path,
        // so the same text reaching this writer twice really writes twice.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&seen);
        let writer = ClipboardWriter::start(Arc::new(move |_selection, text: &str| {
            lock(&recorded).push(text.to_owned());
            Ok(())
        }));

        for _ in 0..2 {
            let (report, reported) = mpsc::channel();
            assert!(
                writer
                    .set_reported(Selection::Clipboard, "same text", move |result| {
                        let _ = report.send(result);
                    })
                    .is_ok()
            );
            assert!(receive(&reported, "the explicit copy").is_ok());
        }

        assert_eq!(*lock(&seen), vec!["same text".to_owned(); 2]);
    }

    #[test]
    fn a_failed_write_does_not_panic_nor_lose_the_next_one() {
        let attempts = Arc::new(Mutex::new(0_u32));
        let counter = Arc::clone(&attempts);
        let writer = ClipboardWriter::start(Arc::new(move |_selection, text: &str| {
            let mut count = lock(&counter);
            *count += 1;
            if text == "first" {
                return Err("the clipboard is held by another application".to_owned());
            }
            Ok(())
        }));

        let (report, reported) = mpsc::channel();
        assert!(
            writer
                .set_reported(Selection::Clipboard, "first", move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert_eq!(
            receive(&reported, "the failed copy").unwrap_err(),
            "the clipboard is held by another application"
        );

        let (report, reported) = mpsc::channel();
        assert!(
            writer
                .set_reported(Selection::Clipboard, "second", move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert!(
            receive(&reported, "the copy after the failure").is_ok(),
            "the worker survives a failed write"
        );
        assert_eq!(*lock(&attempts), 2);
    }
}
