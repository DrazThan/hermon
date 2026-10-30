# Independent supplemental issue 124 review

The original twelve frozen tests passed independently; see
`independent-initial-green.txt`. Production review then found two integration
gaps missed by those initial portable fixtures:

1. Real local roster sessions carry `hr1` Route envelopes. The worker records
   the raw vendor session ID. Comparing these strings directly prevented
   production session navigation. Matching must decode the roster envelope,
   compare full vendor bytes, reject remote origins and malformed envelopes,
   and refuse ambiguous local matches. Both UIs must emit the qualified route
   key to the existing engine pane command.
2. Selecting a pipeline step whose receipt is unavailable retained the previous
   session selection/pane. The UI must clear that unrelated selection, close
   its pane, and show the unavailable-session notice.

The independent reviewer wrote ten supplemental tests before fixes. They are
frozen in `review-freeze.sha256`; the original freeze remains unchanged.

`flow_pipeline_review.rs` verifies five qualified identity/navigation cases plus
two host isolation cases. `review-red.txt` records four failing navigation
tests and three passing guard/host tests. The host tests independently confirm
that a malformed run does not block healthy runs, paused/cancelled runs do not
dispatch, host configuration failures become visible deduplicated events, and
a busy run lock does not block other snapshots or transcript polling.

`flow_pipeline_visual.rs` verifies an actual ratatui frame reached through the
`v` keyboard shortcut, all requested row information, leaving the view with Esc,
and clearing unavailable session selections in both UIs. `visual-red.txt`
records the two stale-selection failures and one passing rendered-frame test.
During test preparation, the GUI fixture was corrected to first open the
unrelated pane through a valid pipeline selection; GUI `drain` alone does not
create selection. The corrected tests were frozen before production fixes.

No production files were edited by the independent reviewer. A manual native
GUI/menubar visual run remains on the original acceptance checklist.
