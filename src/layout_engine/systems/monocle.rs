use objc2_core_foundation::CGRect;
use serde::{Deserialize, Serialize};

use crate::actor::app::{WindowId, pid_t};
use crate::common::collections::{HashMap, HashSet};
use crate::layout_engine::systems::{LayoutSystem, WindowLayoutConstraints};
use crate::layout_engine::utils::compute_tiling_area;
use crate::layout_engine::{Direction, LayoutId, LayoutKind, ResizeOrientation};

/// Single-window fullscreen-ish stacking: every window occupies the whole
/// tiling area and the selection is raised on top (dwm monocle).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct LayoutState {
    windows: Vec<WindowId>,
    selected: Option<WindowId>,
    fullscreen: HashSet<WindowId>,
    fullscreen_within_gaps: HashSet<WindowId>,
}

impl LayoutState {
    fn locate(&self, wid: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| *w == wid)
    }

    fn selected_or_first(&self) -> Option<WindowId> {
        self.selected.or_else(|| self.windows.first().copied())
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.and_then(|wid| self.locate(wid))
    }

    fn remove_window(&mut self, wid: WindowId) -> Option<WindowId> {
        let idx = self.locate(wid)?;
        self.windows.remove(idx);
        self.fullscreen.remove(&wid);
        self.fullscreen_within_gaps.remove(&wid);
        if self.selected == Some(wid) {
            self.selected = self.windows.get(idx).or_else(|| self.windows.last()).copied();
        }
        self.selected
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct MonocleLayoutSystem {
    layouts: slotmap::SlotMap<LayoutId, LayoutState>,
}

impl MonocleLayoutSystem {
    fn layout_state(&self, layout: LayoutId) -> Option<&LayoutState> {
        self.layouts.get(layout)
    }

    fn layout_state_mut(&mut self, layout: LayoutId) -> Option<&mut LayoutState> {
        self.layouts.get_mut(layout)
    }

    fn step_selection(state: &mut LayoutState, direction: Direction) -> Option<WindowId> {
        let idx = state.selected_index()?;
        let next = match direction {
            Direction::Left | Direction::Up => idx.checked_sub(1)?,
            Direction::Right | Direction::Down => {
                (idx + 1 < state.windows.len()).then_some(idx + 1)?
            }
        };
        state.selected = Some(state.windows[next]);
        state.selected
    }
}

impl LayoutSystem for MonocleLayoutSystem {
    fn create_layout(&mut self) -> LayoutId {
        self.layouts.insert(LayoutState::default())
    }

    fn contains_layout(&self, layout: LayoutId) -> bool {
        self.layouts.contains_key(layout)
    }

    fn clone_layout(&mut self, layout: LayoutId) -> LayoutId {
        let cloned = self.layouts.get(layout).cloned().unwrap_or_default();
        self.layouts.insert(cloned)
    }

    fn remove_layout(&mut self, layout: LayoutId) {
        self.layouts.remove(layout);
    }

    fn draw_tree(&self, layout: LayoutId) -> String {
        let Some(state) = self.layouts.get(layout) else {
            return String::new();
        };
        let mut out = String::from("Monocle:");
        for wid in &state.windows {
            if Some(*wid) == state.selected {
                out.push_str(&format!(" [*{:?}]", wid));
            } else {
                out.push_str(&format!(" [{:?}]", wid));
            }
        }
        out.push('\n');
        out
    }

    fn container_tree(&self, layout: LayoutId) -> rift_protocol::ContainerTreeNode {
        let state = self.layouts.get(layout).expect("unknown monocle layout");
        let children = state
            .windows
            .iter()
            .map(|&window| rift_protocol::ContainerTreeNode {
                node_type: rift_protocol::ContainerNodeType::Window,
                layout_kind: None,
                weight: None,
                window_id: Some(window.into()),
                is_selected: state.selected == Some(window),
                is_fullscreen: state.fullscreen.contains(&window),
                is_fullscreen_within_gaps: state.fullscreen_within_gaps.contains(&window),
                role: None,
                pending_split: None,
                children: Vec::new(),
            })
            .collect();
        rift_protocol::ContainerTreeNode {
            node_type: rift_protocol::ContainerNodeType::Container,
            layout_kind: None,
            weight: None,
            window_id: None,
            is_selected: false,
            is_fullscreen: false,
            is_fullscreen_within_gaps: false,
            role: Some("monocle".to_owned()),
            pending_split: None,
            children,
        }
    }

    fn calculate_layout(
        &self,
        layout: LayoutId,
        screen: CGRect,
        _stack_offset: f64,
        constraints: &HashMap<WindowId, WindowLayoutConstraints>,
        gaps: &crate::common::config::GapSettings,
        _stack_line_thickness: f64,
        _stack_line_horiz: crate::common::config::HorizontalPlacement,
        _stack_line_vert: crate::common::config::VerticalPlacement,
    ) -> Vec<(WindowId, CGRect)> {
        let Some(state) = self.layouts.get(layout) else {
            return Vec::new();
        };
        let tiling = compute_tiling_area(screen, gaps);
        // Emit the selection last so back-to-front writers stack it on top.
        let mut ordered: Vec<WindowId> = state
            .windows
            .iter()
            .copied()
            .filter(|wid| Some(*wid) != state.selected)
            .collect();
        if let Some(selected) = state.selected_or_first() {
            ordered.push(selected);
        }
        ordered
            .into_iter()
            .map(|wid| {
                let mut frame = if state.fullscreen.contains(&wid) {
                    screen
                } else {
                    tiling
                };
                if state.fullscreen_within_gaps.contains(&wid) {
                    frame = tiling;
                }
                if let Some(c) = constraints.get(&wid).copied() {
                    let c = c.normalized();
                    let desired_w = c
                        .fixed_for_axis(true)
                        .unwrap_or(frame.size.width)
                        .max(c.min_for_axis(true));
                    let desired_h = c
                        .fixed_for_axis(false)
                        .unwrap_or(frame.size.height)
                        .max(c.min_for_axis(false));
                    let desired_w = if c.max_for_axis(true) > 0.0 {
                        desired_w.min(c.max_for_axis(true))
                    } else {
                        desired_w
                    };
                    let desired_h = if c.max_for_axis(false) > 0.0 {
                        desired_h.min(c.max_for_axis(false))
                    } else {
                        desired_h
                    };
                    frame.size.width = desired_w.min(frame.size.width).max(0.0);
                    frame.size.height = desired_h.min(frame.size.height).max(0.0);
                }
                (wid, frame)
            })
            .collect()
    }

    fn selected_window(&self, layout: LayoutId) -> Option<WindowId> {
        self.layout_state(layout).and_then(|state| state.selected_or_first())
    }

    fn all_windows_in_layout(&self, layout: LayoutId) -> Vec<WindowId> {
        self.layout_state(layout).map(|state| state.windows.clone()).unwrap_or_default()
    }

    fn visible_windows_in_layout(&self, layout: LayoutId) -> Vec<WindowId> {
        self.all_windows_in_layout(layout)
    }

    fn visible_windows_under_selection(&self, layout: LayoutId) -> Vec<WindowId> {
        self.selected_window(layout).into_iter().collect()
    }

    fn ascend_selection(&mut self, _layout: LayoutId) -> bool {
        false
    }

    fn descend_selection(&mut self, _layout: LayoutId) -> bool {
        false
    }

    fn move_focus(
        &mut self,
        layout: LayoutId,
        direction: Direction,
    ) -> (Option<WindowId>, Vec<WindowId>) {
        let Some(state) = self.layout_state_mut(layout) else {
            return (None, vec![]);
        };
        let new_sel = Self::step_selection(state, direction);
        (new_sel, new_sel.into_iter().collect())
    }

    fn window_in_direction(&self, layout: LayoutId, direction: Direction) -> Option<WindowId> {
        let state = self.layout_state(layout)?;
        let idx = state.selected_index()?;
        let next = match direction {
            Direction::Left | Direction::Up => idx.checked_sub(1)?,
            Direction::Right | Direction::Down => {
                (idx + 1 < state.windows.len()).then_some(idx + 1)?
            }
        };
        state.windows.get(next).copied()
    }

    fn add_window_after_selection(&mut self, layout: LayoutId, wid: WindowId) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        if let Some(idx) = state.locate(wid) {
            state.selected = Some(state.windows[idx]);
            return;
        }
        let insert_at = state.selected_index().map(|idx| idx + 1).unwrap_or(state.windows.len());
        state.windows.insert(insert_at.min(state.windows.len()), wid);
        state.selected = Some(wid);
    }

    fn replace_window(&mut self, from: WindowId, to: WindowId) {
        if from == to {
            return;
        }
        for state in self.layouts.values_mut() {
            for window in &mut state.windows {
                if *window == from {
                    *window = to;
                }
            }
            if state.selected == Some(from) {
                state.selected = Some(to);
            }
            if state.fullscreen.remove(&from) {
                state.fullscreen.insert(to);
            }
            if state.fullscreen_within_gaps.remove(&from) {
                state.fullscreen_within_gaps.insert(to);
            }
        }
    }

    fn remove_window(&mut self, wid: WindowId) {
        for state in self.layouts.values_mut() {
            let _ = state.remove_window(wid);
        }
    }

    fn remove_windows_for_app(&mut self, pid: pid_t) {
        for state in self.layouts.values_mut() {
            let windows: Vec<_> = state.windows.iter().copied().filter(|w| w.pid == pid).collect();
            for wid in windows {
                let _ = state.remove_window(wid);
            }
        }
    }

    fn windows_for_app(&self, layout: LayoutId, pid: pid_t) -> Vec<WindowId> {
        self.layout_state(layout)
            .map(|state| state.windows.iter().copied().filter(|w| w.pid == pid).collect())
            .unwrap_or_default()
    }

    fn set_windows_for_app(&mut self, layout: LayoutId, pid: pid_t, desired: Vec<WindowId>) {
        let Some(state) = self.layout_state_mut(layout) else {
            return;
        };
        let selected = state.selected;
        state.windows.retain(|w| w.pid != pid || desired.contains(w));
        for wid in desired.iter().copied().filter(|w| w.pid == pid) {
            if state.locate(wid).is_none() {
                state.windows.push(wid);
            }
        }
        state.selected = selected
            .filter(|wid| state.locate(*wid).is_some())
            .or_else(|| state.windows.first().copied());
    }

    fn has_windows_for_app(&self, layout: LayoutId, pid: pid_t) -> bool {
        self.layout_state(layout)
            .map(|state| state.windows.iter().any(|w| w.pid == pid))
            .unwrap_or(false)
    }

    fn contains_window(&self, layout: LayoutId, wid: WindowId) -> bool {
        self.layout_state(layout).and_then(|state| state.locate(wid)).is_some()
    }

    fn select_window(&mut self, layout: LayoutId, wid: WindowId) -> bool {
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        if state.locate(wid).is_some() {
            state.selected = Some(wid);
            true
        } else {
            false
        }
    }

    fn on_window_resized(
        &mut self,
        _layout: LayoutId,
        _wid: WindowId,
        _old_frame: CGRect,
        _new_frame: CGRect,
        _screen: CGRect,
        _gaps: &crate::common::config::GapSettings,
    ) {
    }

    fn swap_windows(&mut self, layout: LayoutId, a: WindowId, b: WindowId) -> bool {
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        let (Some(a_idx), Some(b_idx)) = (state.locate(a), state.locate(b)) else {
            return false;
        };
        state.windows.swap(a_idx, b_idx);
        true
    }

    fn move_selection(&mut self, layout: LayoutId, direction: Direction) -> bool {
        let Some(state) = self.layout_state_mut(layout) else {
            return false;
        };
        let Some(idx) = state.selected_index() else {
            return false;
        };
        let target = match direction {
            Direction::Left | Direction::Up => idx.checked_sub(1),
            Direction::Right | Direction::Down => {
                (idx + 1 < state.windows.len()).then_some(idx + 1)
            }
        };
        let Some(target) = target else {
            return false;
        };
        state.windows.swap(idx, target);
        true
    }

    fn move_selection_to_layout_after_selection(
        &mut self,
        from_layout: LayoutId,
        to_layout: LayoutId,
    ) {
        let Some(selected) = self.selected_window(from_layout) else {
            return;
        };
        if let Some(state) = self.layout_state_mut(from_layout) {
            let _ = state.remove_window(selected);
        }
        self.add_window_after_selection(to_layout, selected);
    }

    fn split_selection(&mut self, _layout: LayoutId, _kind: LayoutKind) {}

    fn toggle_fullscreen_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some(selected) = state.selected_or_first() else {
            return Vec::new();
        };
        if state.fullscreen.remove(&selected) {
            return vec![selected];
        }
        state.fullscreen_within_gaps.remove(&selected);
        state.fullscreen.insert(selected);
        vec![selected]
    }

    fn toggle_fullscreen_within_gaps_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId> {
        let Some(state) = self.layout_state_mut(layout) else {
            return Vec::new();
        };
        let Some(selected) = state.selected_or_first() else {
            return Vec::new();
        };
        if state.fullscreen_within_gaps.remove(&selected) {
            return vec![selected];
        }
        state.fullscreen.remove(&selected);
        state.fullscreen_within_gaps.insert(selected);
        vec![selected]
    }

    fn has_any_fullscreen_node(&self, layout: LayoutId) -> bool {
        let Some(state) = self.layout_state(layout) else {
            return false;
        };
        !state.fullscreen.is_empty() || !state.fullscreen_within_gaps.is_empty()
    }

    fn join_selection_with_direction(&mut self, _layout: LayoutId, _direction: Direction) {}

    fn apply_stacking_to_parent_of_selection(
        &mut self,
        _layout: LayoutId,
        _default_orientation: crate::common::config::StackDefaultOrientation,
    ) -> Vec<WindowId> {
        Vec::new()
    }

    fn unstack_parent_of_selection(
        &mut self,
        _layout: LayoutId,
        _default_orientation: crate::common::config::StackDefaultOrientation,
    ) -> Vec<WindowId> {
        Vec::new()
    }

    fn parent_of_selection_is_stacked(&self, _layout: LayoutId) -> bool {
        false
    }

    fn unjoin_selection(&mut self, _layout: LayoutId) {}

    fn resize_selection_by(
        &mut self,
        _layout: LayoutId,
        _amount: f64,
        _orientation: ResizeOrientation,
    ) {
    }

    fn rebalance(&mut self, _layout: LayoutId) {}

    fn toggle_tile_orientation(&mut self, _layout: LayoutId) {}
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn w(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn screen() -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1920.0, 1080.0))
    }

    fn layout_frames(
        system: &MonocleLayoutSystem,
        layout: LayoutId,
        screen: CGRect,
    ) -> HashMap<WindowId, CGRect> {
        system
            .calculate_layout(
                layout,
                screen,
                0.0,
                &HashMap::default(),
                &crate::common::config::GapSettings::default(),
                0.0,
                Default::default(),
                Default::default(),
            )
            .into_iter()
            .collect()
    }

    #[test]
    fn single_window_fills_screen_without_gaps() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));

        let frames = layout_frames(&system, layout, screen());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[&w(1)], screen());
    }

    #[test]
    fn stacked_windows_share_one_fullscreen_frame_with_selection_last() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));
        system.add_window_after_selection(layout, w(2));
        system.add_window_after_selection(layout, w(3));
        assert!(system.select_window(layout, w(1)));

        let ordered = system.calculate_layout(
            layout,
            screen(),
            0.0,
            &HashMap::default(),
            &crate::common::config::GapSettings::default(),
            0.0,
            Default::default(),
            Default::default(),
        );
        assert_eq!(ordered.len(), 3);
        for (_, frame) in &ordered {
            assert_eq!(*frame, screen(), "all monocle windows share one frame");
        }
        assert_eq!(ordered.last().map(|(wid, _)| *wid), Some(w(1)));
        assert_eq!(system.visible_windows_in_layout(layout), vec![w(1), w(2), w(3)]);
    }

    #[test]
    fn focus_steps_through_stack_and_stops_at_edges() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));
        system.add_window_after_selection(layout, w(2));
        assert!(system.select_window(layout, w(1)));

        let (focus, raise) = system.move_focus(layout, Direction::Right);
        assert_eq!(focus, Some(w(2)));
        assert_eq!(raise, vec![w(2)]);
        let (focus, _) = system.move_focus(layout, Direction::Right);
        assert_eq!(focus, None, "edge must fall through to cross-space navigation");
        assert_eq!(system.selected_window(layout), Some(w(2)));

        let (focus, _) = system.move_focus(layout, Direction::Left);
        assert_eq!(focus, Some(w(1)));
    }

    #[test]
    fn removal_falls_back_to_neighbor_selection() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));
        system.add_window_after_selection(layout, w(2));
        assert!(system.select_window(layout, w(2)));

        system.remove_window(w(2));
        assert_eq!(system.selected_window(layout), Some(w(1)));
        assert!(!system.contains_window(layout, w(2)));
    }

    #[test]
    fn fullscreen_toggle_round_trip() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));
        assert!(!system.has_any_fullscreen_node(layout));

        assert_eq!(system.toggle_fullscreen_of_selection(layout), vec![w(1)]);
        assert!(system.has_any_fullscreen_node(layout));
        assert_eq!(system.toggle_fullscreen_of_selection(layout), vec![w(1)]);
        assert!(!system.has_any_fullscreen_node(layout));
    }

    #[test]
    fn container_tree_marks_selection() {
        let mut system = MonocleLayoutSystem::default();
        let layout = system.create_layout();
        system.add_window_after_selection(layout, w(1));
        system.add_window_after_selection(layout, w(2));

        let tree = system.container_tree(layout);
        assert_eq!(tree.role.as_deref(), Some("monocle"));
        assert_eq!(tree.children.len(), 2);
        let selected: Vec<_> = tree.children.iter().filter(|node| node.is_selected).collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].window_id, Some(w(2).into()));
    }
}
