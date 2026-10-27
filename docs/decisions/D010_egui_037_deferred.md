# D010 — egui 0.37: evaluated for the editor-groups slate, not taken yet

**Status:** decided by Claude at the founder's request, 2026-10-27 (evaluate 0.37 and make the call) · **Revisit when:** egui-notify and egui-phosphor publish releases on 0.37 and egui_agent is bumped

## Context

egui 0.37.0 shipped while the editor-groups slate (`docs/design/editor_groups.md`) was being
built. The founder asked for an evaluation and a call either way, in the same push.

## What 0.37 changes that matters to Cobalt

- Breaking: `WidgetType` is replaced by `accesskit::Role` (184 call sites in cobalt-app set
  accessibility info through it); `Id::new` and `From<&str> for Id` are deprecated in favour
  of `Id::unique` / salts (61 sites); `Ui::id` becomes `Ui::scope_id`; `Panel` constructors take
  `impl AsIdSalt`; `ViewportCommand::Screenshot` takes a callback (egui_agent's screenshot verb
  uses it); `default_fonts` is no longer a default feature (Cobalt already lists it).
- Useful later: a system font provider on `fontique` (`Context::add_font_provider`) that could
  replace parts of Cobalt's own font manager; `Response::container_*`; a richer accessibility
  tree (panel resize handles, `accessible_name`); window position persistence; kinetic
  scrolling; paste of images.
- Nothing the editor-groups slate needs.

## Dependency readiness (crates.io, 2026-10-27)

| crate | on 0.37? |
| --- | --- |
| egui_table 0.11, egui_plot 0.38, egui_commonmark 0.26, egui_kittest 0.37 | yes |
| egui-notify 0.23 | no (main branch still on 0.36) |
| egui-phosphor 0.14 | no (main branch still on 0.36) |
| egui_agent 0.3.2 (methodify) | no; needs the Role and screenshot-callback changes |
| egui_dock 0.21 | no, and unused: removed from the workspace |

## Decision

Stay on egui 0.36 for this slate. Two direct dependencies have no 0.37 release even on their
main branches, which would mean forking both or replacing toasts and the icon font; egui_agent
is ours to bump but carries the accessibility-role change through every verb; and the app-side
sweep (roles and ids at about 250 sites) is a migration of its own. Coupling that to the
largest UI refactor since the local-query slate would put two kinds of risk in one release for
no feature gain. Revisit as its own iteration once egui-notify and egui-phosphor are on 0.37:
bump egui_agent first, then the workspace, then the role and id sweeps, with the agent scripts
as the regression suite.
