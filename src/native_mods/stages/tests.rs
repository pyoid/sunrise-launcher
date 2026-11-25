use super::*;

#[test]
fn stage_summary_lists_every_stage_in_build_order() {
    let ms = |n| Some(Duration::from_millis(n));
    let stages = Stages {
        reading: ms(1),
        planning: ms(2),
        check: ms(3),
        prebuilt: ms(4),
        toolchain: ms(5),
        package: ms(6),
        staging: ms(7),
        tool: ms(8),
        verify: ms(9),
        install: ms(10),
    };
    assert_eq!(
        stages.summary(),
        "Stage times: reading 1 ms, planning 2 ms, check 3 ms, prebuilt 4 ms, toolchain 5 ms, package 6 ms, staging 7 ms, tool 8 ms, verify 9 ms, install 10 ms"
    );
}

#[test]
fn stage_summary_skips_stages_that_did_not_run() {
    let stages = Stages {
        reading: Some(Duration::from_millis(12)),
        planning: Some(Duration::from_millis(40)),
        check: Some(Duration::from_millis(300)),
        ..Stages::default()
    };
    assert_eq!(
        stages.summary(),
        "Stage times: reading 12 ms, planning 40 ms, check 300 ms"
    );
}
