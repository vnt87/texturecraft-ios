//! File services the app binary installs for platforms whose dialogs answer on a later frame.
//!
//! The desktop shell calls `rfd` where it needs a path, the browser uses the File System Access
//! API, and iPadOS presents a UIKit picker from its runner. Only the last of those needs a hook:
//! the runner installs [`Services`] at startup and the shell asks it to pick or share a file.
//!
//! A picker never blocks the frame: the runner answers with the file's bytes on the callback it is
//! handed, and [`crate::web_files::poll`] turns them into session work on the next frame.

use std::sync::{Arc, Mutex, PoisonError};

/// What a picker should show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The dialog's label for the file type, already translated.
    pub filter: String,
    /// Accepted filename extensions, without dots.
    pub extensions: Vec<String>,
    /// Whether the user may pick more than one file.
    pub multiple: bool,
}

/// Picks files and hands `(name, bytes)` for each one to `done`. A picker the user cancels calls
/// `done` with no files.
pub type PickFn = dyn Fn(Request, Box<dyn FnOnce(Vec<(String, Vec<u8>)>) + Send>) + Send + Sync;

/// Offers a finished file to the user so they can keep it (iPadOS: the export picker).
pub type ShareFn = dyn Fn(String, Vec<u8>) -> Result<(), String> + Send + Sync;

/// The platform hooks the runner installed. A missing hook makes the action fail with a message
/// instead of hanging the frame.
#[derive(Default)]
pub struct Services {
    /// An open picker, when the platform has one.
    pub pick: Option<Box<PickFn>>,
    /// An export picker, when the platform can hand a file out.
    pub share: Option<Box<ShareFn>>,
}

static SERVICES: Mutex<Option<Arc<Services>>> = Mutex::new(None);

/// Install the runner's services, replacing any earlier ones.
pub fn install(services: Services) {
    *SERVICES.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(services));
}

/// The installed services, if any.
pub fn current() -> Option<Arc<Services>> {
    SERVICES.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Whether a picker is installed. Menus use it to keep an action that can never answer hidden.
pub fn can_pick() -> bool {
    matches!(current(), Some(services) if services.pick.is_some())
}

/// Present an open dialog. `done` receives the picked files; a platform without a hook reports why.
pub fn pick(request: Request, done: impl FnOnce(Result<Vec<(String, Vec<u8>)>, String>) + Send + 'static) {
    let Some(services) = current() else {
        return done(Err(UNAVAILABLE.into()));
    };
    let Some(pick) = services.pick.as_ref() else {
        return done(Err(UNAVAILABLE.into()));
    };
    pick(request, Box::new(move |files| done(Ok(files))));
}

/// Hand a finished file to the user.
pub fn share(name: &str, bytes: &[u8]) -> Result<(), String> {
    let Some(services) = current() else {
        return Err(UNAVAILABLE.into());
    };
    let Some(share) = services.share.as_ref() else {
        return Err(UNAVAILABLE.into());
    };
    share(name.to_string(), bytes.to_vec())
}

/// Why a file action cannot run: shown as a toast.
const UNAVAILABLE: &str = "This iPad build has no file picker installed yet.";

#[cfg(test)]
mod tests {
    use super::*;

    /// What the installed share hook was handed.
    static SHARED: Mutex<Option<String>> = Mutex::new(None);

    type Picked = Result<Vec<(String, Vec<u8>)>, String>;

    /// [`pick`] hands its answer to a `'static` callback, so a test collects it in a shared slot.
    fn asked(request: Request) -> Picked {
        let slot: Arc<Mutex<Option<Picked>>> = Arc::new(Mutex::new(None));
        let out = Arc::clone(&slot);
        pick(request, move |r| *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(r));
        slot.lock().unwrap_or_else(PoisonError::into_inner).take().unwrap_or_else(|| Err("the picker never answered".into()))
    }

    /// The hooks are process-wide, so both states are checked by one test, in order, and no hook
    /// is left installed.
    #[test]
    fn hooks_answer_when_installed_and_explain_when_not() {
        install(Services::default());
        assert!(!can_pick());
        assert_eq!(share("project.tcraft", b"x").unwrap_err(), UNAVAILABLE.to_string());
        assert_eq!(asked(Request { filter: "Meshes".into(), extensions: vec!["glb".into()], multiple: false }), Err(UNAVAILABLE.into()));

        install(Services {
            pick: Some(Box::new(|request, done| done(vec![(request.filter, vec![1, 2])]))),
            share: Some(Box::new(|name, bytes| {
                *SHARED.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("{name}:{}", bytes.len()));
                Ok(())
            })),
        });
        assert!(can_pick());
        assert_eq!(asked(Request { filter: "Meshes".into(), extensions: vec![], multiple: false }), Ok(vec![("Meshes".to_string(), vec![1, 2])]));
        share("a.png", &[7, 8]).unwrap();
        assert_eq!(*SHARED.lock().unwrap_or_else(PoisonError::into_inner), Some("a.png:2".to_string()));

        install(Services::default());
        assert!(share("a.png", &[]).is_err(), "a build with no export picker says so");
    }
}
