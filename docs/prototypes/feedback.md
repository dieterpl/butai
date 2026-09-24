# BOOTH feedback and implementation plan

Status: the user authorized terminal implementation and publishing the next beta. Implemented from an isolated checkout as 1.3.0-dev.3.beta.2; published at https://github.com/dieterpl/butai/releases/tag/v1.3.0-dev.3.beta.2 (commit 4b9444e). Mock-ups and this feedback list stay local and are excluded from the release commit.

## Feedback list

1. Match the real terminal app: character grid, default palette, box-drawing borders, workspace chips, and dense rows. Accepted direction.
2. Add a faint tree spine connecting machines, projects, and agents in FLEET. Accepted direction.
3. Collapsed COMPUTE: one or two lines per machine. Current mock-up uses one line: name, agent count, highest resource usage. Expanded COMPUTE retains the preceding mock-up's detail view. Actual implementation must reuse the app's existing expanded renderer.
4. Double-click machine or project names in BOOTH to fold/unfold their children. Single click selects/previews; chevron and keyboard folding remain available. Double-click does not spawn, close a workspace, or trigger explicit action buttons. Interpretation for this mock-up: “opens and closes” means expanding/collapsing the tree.
5. Replace the busy working-agent figures with a quiet loading indicator. Current experiment: three fixed character cells cycling `.`, `..`, `...` every 1.2 seconds. Other agent states use simple markers. Await feedback on this experiment.

6. Remove all remaining figure-style sprites, including `?o?` and `-o-`. Current mock-up uses `!` for waiting, `·` for idle, `✓` for done, and slow loading dots for working. Each indicator occupies three character cells; a legend explains the markers.

7. Clicking an agent chat or NEEDS YOU entry in BOOTH should give its stage keyboard focus immediately. Machine and project selection keeps focus on FLEET. Implemented and verified with real terminal input.

## Implementation plan (now completed)

- Extend fleet rendering in `crates/butai-client/src/chrome/mod.rs` with tree branch characters derived from visible machine/project/agent rows and folds. Preserve selection backgrounds and action hit regions.
- Reduce collapsed COMPUTE to a summary (optionally a second line if readability needs it); retain existing expanded `draw_system` path and worst-reading calculation. Update shared height and hit-test calculations together.
- Recognize double-clicks in the client mouse handling using elapsed time and stable machine/project identity, since terminal input provides individual mouse events. Clear pending clicks on navigation or target changes. Route folding through existing fold actions; keep explicit buttons independent.
- Replace working-state sprites with a fixed-width loading indicator and a slower animation cadence. Review its use in the tray, fleet, folded project summaries, and workspace agent rail for consistency.
- Verify tree endings after folding, accurate compute hit targets, repeated double-clicks, isolated single clicks, action-button behavior, and keyboard equivalents. Update relevant workbench/key documentation after implementation.

## Review links

- `booth.html`: interactive mock-up; uses sample data and simulated actions.
- `booth.png`: collapsed compute preview.
- `booth-expanded.png`: expanded compute preview.

Production changes are tracked separately in the release checkout; these prototype files are not published.
