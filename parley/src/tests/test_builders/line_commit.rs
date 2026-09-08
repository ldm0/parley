// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;

use super::ColorBrush;
use super::custom_box_fit::shape;
use crate::{BaseDirection, Line, LineEdgeWhitespace, YieldData};

fn cluster_geometry(
    line: Line<'_, ColorBrush>,
) -> Vec<(core::ops::Range<usize>, f32, Option<f32>)> {
    line.runs()
        .flat_map(|run| {
            run.visual_clusters()
                .map(|cluster| {
                    assert_eq!(cluster.line().text_range(), line.text_range());
                    (
                        cluster.text_range(),
                        cluster.advance(),
                        cluster.visual_offset(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn completed_line_views_share_the_same_clusters_before_and_after_finishing() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let mut layout = shape("WW WW WW", LineEdgeWhitespace::Collapse, direction, &[]);
        let word = shape("WW", LineEdgeWhitespace::Collapse, direction, &[])
            .calculate_content_widths()
            .max;
        let mut snapshots = Vec::new();
        {
            let mut breaker = layout.break_lines();
            assert!(breaker.last_line().is_none());
            breaker.state_mut().set_layout_max_advance(word + 10.0);
            breaker.state_mut().set_line_max_advance(word + 10.0);
            while let Some(YieldData::LineBreak(_)) = breaker.break_next() {
                let line = breaker.last_line().unwrap();
                assert_eq!(line.is_rtl(), direction == BaseDirection::Rtl);
                snapshots.push(cluster_geometry(line));
            }
        }
        assert_eq!(snapshots.len(), 3);
        assert_eq!(
            snapshots,
            layout.lines().map(cluster_geometry).collect::<Vec<_>>()
        );
    }
}

#[test]
fn caller_resolved_block_advance_preserves_font_metrics_and_counts_exclusion_gaps() {
    let mut layout = shape(
        "WW WW WW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[],
    );
    let word = shape("WW", LineEdgeWhitespace::Collapse, BaseDirection::Ltr, &[])
        .calculate_content_widths()
        .max;
    {
        let mut breaker = layout.break_lines();
        breaker.state_mut().set_layout_max_advance(word + 10.0);
        breaker.state_mut().set_line_max_advance(word + 10.0);
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        let font_metrics = *breaker.last_line().unwrap().metrics();
        let clusters = cluster_geometry(breaker.last_line().unwrap());
        breaker.set_last_line_block_advance(50.0);
        assert_eq!(breaker.last_line().unwrap().block_advance(), 50.0);
        assert_eq!(breaker.state().line_y(), 50.0);
        assert_eq!(*breaker.last_line().unwrap().metrics(), font_metrics);
        assert_eq!(cluster_geometry(breaker.last_line().unwrap()), clusters);
        // The line-layout caller moves the following line past an exclusion.
        breaker.state_mut().set_line_y(70.0);
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        let second = breaker.last_line().unwrap();
        assert_eq!(second.block_offset(), 70.0);
        let first_cluster = second.runs().next().unwrap().get(0).unwrap();
        assert_eq!(first_cluster.previous_logical().unwrap().text_range(), 2..3);
        assert_eq!(first_cluster.previous_visual().unwrap().text_range(), 2..3);
        breaker.set_last_line_block_advance(30.0);
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        assert_eq!(breaker.last_line().unwrap().block_offset(), 100.0);
        assert_eq!(breaker.last_line().unwrap().block_advance(), 24.0);
        assert!(breaker.break_next().is_none());
    }
    assert_eq!(layout.height(), 124.0);
}

#[test]
fn zero_block_advance_and_rewind_preserve_the_completed_line_collection() {
    let mut layout = shape(
        "\nWW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[],
    );
    {
        let mut breaker = layout.break_lines();
        breaker.state_mut().set_layout_max_advance(200.0);
        breaker.state_mut().set_line_max_advance(200.0);
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        breaker.set_last_line_block_advance(0.0);
        let checkpoint = breaker.state().clone();
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        assert_eq!(breaker.last_line().unwrap().block_offset(), 0.0);
        breaker.revert_to(checkpoint);
        assert_eq!(breaker.last_line().unwrap().text_range(), 0..1);
        assert_eq!(breaker.last_line().unwrap().block_advance(), 0.0);
        assert!(matches!(
            breaker.break_next(),
            Some(YieldData::LineBreak(_))
        ));
        assert_eq!(breaker.last_line().unwrap().text_range(), 1..3);
        breaker.set_last_line_block_advance(40.0);
        assert!(breaker.break_next().is_none());
    }
    assert_eq!(layout.len(), 2);
    assert_eq!(layout.height(), 40.0);
}
