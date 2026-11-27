use super::*;

#[test]
fn results_keep_item_order_for_any_worker_count() {
    let items: Vec<usize> = (0..200).collect();
    for workers in [1, 2, 7] {
        let doubled = run(&items, workers, None, None, || (), |_, n| Ok(n * 2)).unwrap();
        assert_eq!(doubled, items.iter().map(|n| n * 2).collect::<Vec<_>>());
    }
}

#[test]
fn a_worker_panic_is_an_error_not_a_panic_on_the_caller() {
    let items: Vec<usize> = (0..50).collect();
    for workers in [1, 2] {
        let error = run(
            &items,
            workers,
            None,
            None,
            || (),
            |_, n| {
                if *n == 20 {
                    panic!("injected");
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("panicked"), "{error}");
    }
}

#[test]
fn the_first_error_stops_the_other_workers() {
    let items: Vec<usize> = (0..10_000).collect();
    let ran = AtomicUsize::new(0);
    let error = run(
        &items,
        2,
        None,
        None,
        || (),
        |_, n| {
            ran.fetch_add(1, Ordering::Relaxed);
            if *n == 3 {
                return Err(Error::Input("refused".into()));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "refused");
    assert!(ran.load(Ordering::Relaxed) < items.len());
}

#[test]
fn a_stopped_attempt_returns_the_cancellation() {
    let stop = Cancellation::default();
    stop.stop();
    let items = [1, 2, 3];
    let error = run(&items, 2, Some(&stop), None, || (), |_, _| Ok(())).unwrap_err();
    assert_eq!(Some(error.to_string()), stop.check().err());
}
