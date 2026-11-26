//! Failures reported from outside a call reach the error screen, once, with one wake.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gogo_gui_model::fake::FakeBackend;
use gogo_gui_model::{ErrorView, Modal, Model};

#[test]
fn a_reported_failure_becomes_an_error_screen_once() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    let mut model = Model::new(FakeBackend::new(), move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    model.pump();
    let failures = model.failures();
    let error = ErrorView::failed("The library could not be read", "offline");
    let from_elsewhere = {
        let (failures, error) = (failures.clone(), error.clone());
        std::thread::spawn(move || {
            failures.report(error.clone());
            failures.report(error);
        })
    };
    from_elsewhere.join().expect("reporter");
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert!(model.modal().is_none(), "nothing before the pump");

    model.pump();
    match model.modal() {
        Some(Modal::Error { error: shown, .. }) => assert_eq!(*shown, error),
        other => panic!("{other:?}"),
    }
    model.dismiss();
    model.pump();
    assert!(model.modal().is_none(), "only one was queued");
}
