//! Worker-local diagnostic routing. Native engine workers collect notices for
//! the existing context event; headless readers keep their stderr diagnostics.
//! This never changes process file descriptors or another thread's policy.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

const LIMIT: usize = 128;
const OVERFLOW: &str = "additional source diagnostics omitted: notice limit";

struct Notice {
    text: String,
    hermes_database: bool,
}

thread_local! {
    static NOTICES: RefCell<Option<Vec<Notice>>> = const { RefCell::new(None) };
}

/// Scoped to the worker that owns the sources, including their construction.
/// Not Send: restoring another thread's policy would be incorrect.
pub struct Capture {
    previous: Option<Vec<Notice>>,
    _thread: PhantomData<Rc<()>>,
}

pub fn capture() -> Capture {
    Capture {
        previous: NOTICES.with(|notices| notices.replace(Some(Vec::new()))),
        _thread: PhantomData,
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        NOTICES.with(|notices| notices.replace(self.previous.take()));
    }
}

pub fn capturing() -> bool {
    NOTICES.with(|notices| notices.borrow().is_some())
}

pub fn report(message: &str) {
    emit(message, false);
}

pub(crate) fn hermes_database(message: &str) {
    emit(message, true);
}

/// The profile registry supplies authoritative owner-labelled health notices.
/// Do not spend native warning slots on duplicate lower-level DB failures.
pub fn registry_health_reported() {
    NOTICES.with(|notices| {
        if let Some(notices) = notices.borrow_mut().as_mut() {
            notices.retain(|notice| !notice.hermes_database);
        }
    });
}

fn emit(message: &str, hermes_database: bool) {
    let message = crate::render::clip(&crate::render::sanitize(message), 256);
    NOTICES.with(|notices| {
        if let Some(notices) = notices.borrow_mut().as_mut() {
            if !notices.iter().any(|notice| notice.text == message) {
                if notices.len() < LIMIT {
                    notices.push(Notice {
                        text: message,
                        hermes_database,
                    });
                } else if let Some(last) = notices.last_mut() {
                    *last = Notice {
                        text: OVERFLOW.into(),
                        hermes_database: false,
                    };
                }
            }
        } else {
            eprintln!("{message}");
        }
    });
}

/// Drain once per scan. Active failures are reasserted by source readers;
/// recovered failures therefore disappear rather than becoming stale notices.
pub fn take() -> Vec<String> {
    NOTICES.with(|notices| {
        notices
            .borrow_mut()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
            .into_iter()
            .map(|notice| notice.text)
            .collect()
    })
}
