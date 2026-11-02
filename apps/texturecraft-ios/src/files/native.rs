//! Audited UIKit boundary. Pickers are presented on the main queue; reading a provider's file runs
//! on a worker thread, so the event loop never waits on it.
use super::{bounded_read, export_dir, remaining_limit, safe_name};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSError, NSFileCoordinator, NSFileCoordinatorReadingOptions, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController, UIViewController};
use objc2_uniform_type_identifiers::UTType;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use texturecraft_ui_egui::services::{Request, Services};

thread_local! {
    // UIKit holds its delegate weakly: keep both objects until the terminal callback.
    static ACTIVE: RefCell<Option<(Retained<PickerDelegate>, Retained<UIDocumentPickerViewController>)>> = const { RefCell::new(None) };
}

/// The picker's answer: each chosen file as a name and its bytes.
type Picked = Vec<(String, Vec<u8>)>;

/// What the picker does with its answer. `Export` keeps the staged file so the stage is cleared
/// whether the user placed the file or cancelled.
enum Completion {
    Open(Box<dyn FnOnce(Picked) + Send>),
    Export(PathBuf),
}

impl Completion {
    /// The picker could not be presented: tell the shell it got nothing rather than hang it.
    fn fail(self, error: String) {
        match self {
            Completion::Open(done) => {
                log::warn!("file picker unavailable: {error}");
                done(Vec::new());
            }
            Completion::Export(staged) => {
                log::warn!("export picker unavailable: {error}");
                clear_stage(&staged);
            }
        }
    }
}

struct PickerState {
    completion: RefCell<Option<Completion>>,
}

define_class!(
    // SAFETY: NSObject adds no subclass invariants. This class lives on the main thread and owns
    // only Rust ivars; UIKit calls the declared signatures on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PickerState]
    struct PickerDelegate;
    // SAFETY: NSObjectProtocol adds no implementation invariants.
    unsafe impl NSObjectProtocol for PickerDelegate {}
    // SAFETY: Every callback runs on UIKit's main thread and its arguments stay borrowed only for
    // the call. Selected URLs are retained before crossing to a worker.
    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn picked(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            let urls: Vec<Retained<NSURL>> = urls.iter().collect();
            if let Some(completion) = self.ivars().completion.borrow_mut().take() {
                match completion {
                    Completion::Open(done) => {
                        let spawned = std::thread::Builder::new().name("texturecraft-document-import".into()).spawn(move || done(read_selection(&urls)));
                        if let Err(error) = spawned {
                            // The shell simply sees no import and the user can ask again.
                            log::error!("could not start the document import: {error}");
                        }
                    }
                    Completion::Export(staged) => {
                        // UIKit copied the file to the user's destination; the stage is ours.
                        clear_stage(&staged);
                    }
                }
            }
            release_picker();
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            if let Some(completion) = self.ivars().completion.borrow_mut().take() {
                match completion {
                    Completion::Open(done) => done(Vec::new()),
                    Completion::Export(staged) => clear_stage(&staged),
                }
            }
            release_picker();
        }
    }
);

/// Release the picker after UIKit returns from its own callback.
fn release_picker() {
    dispatch2::DispatchQueue::main().exec_async(|| {
        ACTIVE.with(|active| active.borrow_mut().take());
    });
}

fn clear_stage(staged: &Path) {
    if let Err(error) = std::fs::remove_file(staged) {
        log::warn!("could not clear the staged export {}: {error}", staged.display());
    }
}

fn root_controller(mtm: MainThreadMarker) -> Result<Retained<UIViewController>, String> {
    let window = crate::scenes::window(mtm).ok_or("no active UIKit window")?;
    let root = window.rootViewController().ok_or("the UIKit window has no root controller")?;
    if root.presentedViewController().is_some() {
        return Err("another UIKit dialog is already open".into());
    }
    Ok(root)
}

/// Present the picker, or report on the completion why the answer will be empty.
fn show(picker: Retained<UIDocumentPickerViewController>, completion: Completion, mtm: MainThreadMarker) {
    let root = match root_controller(mtm) {
        Ok(root) => root,
        Err(error) => return completion.fail(error),
    };
    if ACTIVE.with(|active| active.borrow().is_some()) {
        return completion.fail("a document picker is already open".into());
    }
    let allocated = PickerDelegate::alloc(mtm).set_ivars(PickerState { completion: RefCell::new(Some(completion)) });
    // SAFETY: `init` initializes the allocated NSObject subclass on the main thread; the Rust ivars
    // are already set and ownership of the result transfers to the Retained.
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![super(allocated), init] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    ACTIVE.with(|active| *active.borrow_mut() = Some((delegate, picker.clone())));
    root.presentViewController_animated_completion(&picker, true, None);
}

/// Open a picker for the request's kinds and answer with the files the user chose.
fn present(request: Request, done: impl FnOnce(Picked) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return Completion::Open(Box::new(done)).fail("UIKit's main queue is unavailable".into());
        };
        let types = NSArray::from_retained_slice(&content_types(&request));
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(UIDocumentPickerViewController::alloc(mtm), &types, false);
        picker.setAllowsMultipleSelection(request.multiple);
        show(picker, Completion::Open(Box::new(done)), mtm);
    });
}

/// The kinds the picker shows: the request's extensions, or every file when the system has no
/// declaration for one of them.
fn content_types(request: &Request) -> Vec<Retained<UTType>> {
    let mut types: Vec<Retained<UTType>> = Vec::new();
    let mut unknown = Vec::new();
    for extension in &request.extensions {
        let key = NSString::from_str(extension.trim_start_matches('.'));
        match UTType::typeWithFilenameExtension(&key) {
            Some(kind) => types.push(kind),
            None => unknown.push(extension.as_str()),
        }
    }
    if !unknown.is_empty() {
        log::warn!("no system type for {}; offering every file", unknown.join(", "));
    }
    if (types.is_empty() || !unknown.is_empty())
        && let Some(data) = UTType::typeWithIdentifier(&NSString::from_str("public.data"))
    {
        types.push(data);
    }
    types
}

/// Reading a provider file is scoped to the retained URL and balanced even when the read fails.
struct SecurityAccess {
    url: Retained<NSURL>,
    started: bool,
}

impl SecurityAccess {
    fn new(url: Retained<NSURL>) -> Self {
        // SAFETY: NSURL is immutable and Send + Sync; Foundation allows security-scope calls off
        // the main thread, and Drop balances only a successful start.
        let started = unsafe { url.startAccessingSecurityScopedResource() };
        Self { url, started }
    }
}

impl Drop for SecurityAccess {
    fn drop(&mut self) {
        if self.started {
            // SAFETY: The retained URL is alive and this guard owns exactly one successful start.
            unsafe { self.url.stopAccessingSecurityScopedResource() };
        }
    }
}

/// Every picked document, as `(name, bytes)`, up to the import budget.
fn read_selection(urls: &[Retained<NSURL>]) -> Vec<(String, Vec<u8>)> {
    const MAX_SELECTED: usize = 64;
    if urls.len() > MAX_SELECTED {
        log::error!("too many documents selected ({}); pick at most {MAX_SELECTED}", urls.len());
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut read_total = 0u64;
    for url in urls {
        let Ok(limit) = remaining_limit(read_total) else {
            log::error!("the selection is over the import limit; ignoring the remaining files");
            break;
        };
        let access = SecurityAccess::new(url.clone());
        match coordinated_read(&access.url, limit) {
            Ok((name, bytes)) => match read_total.checked_add(bytes.len() as u64) {
                Some(total) => {
                    read_total = total;
                    files.push((name, bytes));
                }
                None => log::error!("{} is too large to import", name),
            },
            Err(error) => log::error!("could not import a picked document: {error}"),
        }
    }
    files
}

/// Read one provider URL through the file coordinator so a syncing provider is not disturbed,
/// capped by the import budget left for it.
fn coordinated_read(url: &NSURL, limit: u64) -> Result<(String, Vec<u8>), String> {
    let name = url.lastPathComponent().map(|component| safe_name(&component.to_string())).unwrap_or_else(|| safe_name(""));
    let outcome = RefCell::new(None);
    let reader = block2::RcBlock::new(|target: std::ptr::NonNull<NSURL>| {
        // SAFETY: Foundation hands a live immutable NSURL to this synchronous accessor and never
        // uses it after the block returns; nothing borrowed escapes.
        let target = unsafe { target.as_ref() };
        let path = target.path().map(|path| PathBuf::from(path.to_string())).ok_or_else(|| "the provider returned no local path".to_string());
        let result = path.and_then(|path| bounded_read(&path, limit));
        *outcome.borrow_mut() = Some(result);
    });
    let mut error: Option<Retained<NSError>> = None;
    NSFileCoordinator::new().coordinateReadingItemAtURL_options_error_byAccessor(url, NSFileCoordinatorReadingOptions::empty(), Some(&mut error), &reader);
    if let Some(error) = error {
        return Err(error.to_string());
    }
    drop(reader);
    outcome.into_inner().ok_or("the file provider did not allow reading")?.map(|bytes| (name, bytes))
}

/// Offer a finished file to the user: stage it in the container, then let the picker place it.
fn share(name: &str, bytes: &[u8]) -> Result<(), String> {
    let stage = staged_path(name)?;
    std::fs::create_dir_all(stage.parent().ok_or("the export stage has no folder")?).map_err(|e| e.to_string())?;
    let temp = stage.with_extension("part");
    std::fs::write(&temp, bytes).map_err(|e| format!("could not stage {name}: {e}"))?;
    std::fs::rename(&temp, &stage).map_err(|e| format!("could not stage {name}: {e}"))?;
    let staged = stage.clone();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return Completion::Export(staged).fail("UIKit's main queue is unavailable".into());
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&stage.display().to_string()));
        let urls = NSArray::from_retained_slice(&[url]);
        let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(UIDocumentPickerViewController::alloc(mtm), &urls, true);
        show(picker, Completion::Export(staged), mtm);
    });
    Ok(())
}

/// Where a finished file waits for the user.
fn staged_path(name: &str) -> Result<PathBuf, String> {
    let dir = export_dir(std::env::var_os("HOME").as_deref())?;
    Ok(dir.join(safe_name(name)))
}

/// Hand the shell the UIKit pickers.
pub fn install() {
    texturecraft_ui_egui::services::install(Services { pick: Some(Box::new(present)), share: Some(Box::new(|name, bytes| share(&name, &bytes))) });
}
