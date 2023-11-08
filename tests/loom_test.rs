use loom::thread;

#[test]
#[should_panic]
fn loom_test() {
    loom::model(|| {
        let t1 = thread::spawn(|| {});
        t1.join()
    })
}
