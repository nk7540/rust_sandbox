use loom::sync::atomic::AtomicUsize;
use loom::thread;

use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;

#[test]
fn test_concurrent_logic() {
    loom::model(|| {
        let v1 = Arc::new(AtomicUsize::new(0));
        println!("{}", v1.load(SeqCst));
        let v2 = v1.clone();

        thread::spawn(move || {
            v1.store(1, SeqCst);
        });
        thread::spawn(move || {});
        // v1.store(1, SeqCst);

        assert_eq!(0, v2.load(SeqCst));
    });
}
