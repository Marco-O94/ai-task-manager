//! Pure board reducers, host-tested with `cargo test -p atm-ui` (spec §9.3). Owner: M2-UI-BOARD.

use atm_types::{Id, Task, TaskCard, TaskStatus};

/// Gap between consecutive positions: appends use `max + GAP`, renumbering `k * GAP`.
const GAP: f64 = 1024.0;
/// Neighbours closer than this make the whole column renumber (spec §5.2).
const EPSILON: f64 = 1e-6;

/// A row carrying a task: the board's cards, the mock backend's tasks.
pub trait HasTask {
    fn task(&self) -> &Task;
    fn task_mut(&mut self) -> &mut Task;
}

impl HasTask for Task {
    fn task(&self) -> &Task {
        self
    }
    fn task_mut(&mut self) -> &mut Task {
        self
    }
}

impl HasTask for TaskCard {
    fn task(&self) -> &Task {
        &self.task
    }
    fn task_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

/// Cards of one column, in position order.
pub fn column(cards: &[TaskCard], status: TaskStatus) -> Vec<TaskCard> {
    let mut col: Vec<TaskCard> = cards
        .iter()
        .filter(|c| c.task.status == status)
        .cloned()
        .collect();
    col.sort_by(|a, b| a.task.position.total_cmp(&b.task.position));
    col
}

/// Every card in board order: column by column (`TaskStatus::ALL`), each in position order.
/// The list view's fixed order (spec F4); `get_board` sorts by position only.
pub fn by_column(cards: &[TaskCard]) -> Vec<TaskCard> {
    TaskStatus::ALL
        .iter()
        .flat_map(|&status| column(cards, status))
        .collect()
}

/// Optimistic local reorder mirroring `move_task`: `id` goes into `status` before `before_id`
/// (at the end when `None`). The `changed` refetch that follows is authoritative.
#[allow(clippy::ptr_arg)] // frozen M1 signature
pub fn apply_move(
    cards: &mut Vec<TaskCard>,
    id: &str,
    status: TaskStatus,
    before_id: Option<&str>,
) {
    move_to(cards, id, status, before_id);
}

/// `move_task` with the backend's position rule (spec §5.2): the midpoint with the
/// predecessor (0 above the first task), `max + GAP` at the end, and the column (same project,
/// same status) renumbered by `GAP` when two neighbours get closer than `EPSILON`. `false`,
/// with nothing changed, if `id` is unknown or `before_id` is not in the destination column.
pub fn move_to<T: HasTask>(
    rows: &mut [T],
    id: &str,
    status: TaskStatus,
    before_id: Option<&str>,
) -> bool {
    let Some(from) = rows.iter().position(|r| r.task().id == id) else {
        return false;
    };
    let project = rows[from].task().project_id.clone();
    // Destination column without the moved row, in position order.
    let mut col: Vec<usize> = (0..rows.len())
        .filter(|&i| {
            let t = rows[i].task();
            i != from && t.status == status && t.project_id == project
        })
        .collect();
    col.sort_by(|&a, &b| rows[a].task().position.total_cmp(&rows[b].task().position));
    let at = match before_id {
        None => col.len(),
        Some(before) => match col.iter().position(|&i| rows[i].task().id == before) {
            Some(at) => at,
            None => return false,
        },
    };
    let prev = at
        .checked_sub(1)
        .map_or(0.0, |k| rows[col[k]].task().position);
    let next = col.get(at).map(|&i| rows[i].task().position);

    rows[from].task_mut().status = status;
    match next {
        None => rows[from].task_mut().position = prev + GAP,
        Some(next) if next - prev >= EPSILON => {
            rows[from].task_mut().position = (prev + next) / 2.0;
        }
        Some(_) => {
            col.insert(at, from);
            for (k, &i) in col.iter().enumerate() {
                rows[i].task_mut().position = (k + 1) as f64 * GAP;
            }
        }
    }
    true
}

/// `before_id` for a drop at insertion `index` of `column` (the cards as rendered, the dragged
/// one included): `Some(None)` is the end of the column, `None` a drop that leaves the order
/// unchanged (right above or below the dragged card itself).
pub fn drop_before(column: &[TaskCard], index: usize, dragged: &str) -> Option<Option<Id>> {
    let before = column[index.min(column.len())..]
        .iter()
        .map(|c| &c.task.id)
        .find(|id| *id != dragged)
        .cloned();
    let unchanged = column
        .iter()
        .position(|c| c.task.id == dragged)
        .is_some_and(|i| column.get(i + 1).map(|c| &c.task.id) == before.as_ref());
    (!unchanged).then_some(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: [TaskStatus; 3] = [TaskStatus::Todo, TaskStatus::InProgress, TaskStatus::Done];

    fn task(id: &str, project: &str, status: TaskStatus, position: f64) -> Task {
        Task {
            id: id.into(),
            project_id: project.into(),
            title: id.into(),
            description: String::new(),
            status,
            position,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card(id: &str, status: TaskStatus, position: f64) -> TaskCard {
        TaskCard {
            task: task(id, "p", status, position),
            attempt_id: None,
            attempt_state: None,
            branch: None,
            running: false,
            pending_approvals: 0,
            last_status: None,
            last_stop_reason: None,
            worktree_state: None,
        }
    }

    fn ids(cards: &[TaskCard], status: TaskStatus) -> Vec<String> {
        column(cards, status)
            .into_iter()
            .map(|c| c.task.id)
            .collect()
    }

    fn board() -> Vec<TaskCard> {
        vec![
            card("c", TaskStatus::Todo, 3072.0),
            card("x", TaskStatus::Done, 1024.0),
            card("a", TaskStatus::Todo, 1024.0),
            card("b", TaskStatus::Todo, 2048.0),
        ]
    }

    #[test]
    fn column_filters_and_sorts_by_position() {
        let cards = board();
        assert_eq!(ids(&cards, TaskStatus::Todo), ["a", "b", "c"]);
        assert_eq!(ids(&cards, TaskStatus::Done), ["x"]);
        assert!(column(&cards, TaskStatus::Cancelled).is_empty());
    }

    #[test]
    fn by_column_follows_the_columns_then_the_positions() {
        let mut cards = board();
        cards.push(card("r", TaskStatus::InReview, 1024.0));
        cards.push(card("k", TaskStatus::Cancelled, 512.0));
        cards.push(card("p", TaskStatus::InProgress, 4096.0));
        cards.push(card("q", TaskStatus::InProgress, 10.0));
        let order: Vec<String> = by_column(&cards).into_iter().map(|c| c.task.id).collect();
        assert_eq!(order, ["a", "b", "c", "q", "p", "r", "x", "k"]);
        assert!(by_column(&[]).is_empty());
    }

    #[test]
    fn move_to_the_end_of_another_column_appends_after_the_max() {
        let mut cards = board();
        apply_move(&mut cards, "a", TaskStatus::Done, None);
        assert_eq!(ids(&cards, TaskStatus::Todo), ["b", "c"]);
        assert_eq!(ids(&cards, TaskStatus::Done), ["x", "a"]);
        let a = &cards[2].task;
        assert_eq!((a.status, a.position), (TaskStatus::Done, 2048.0));
    }

    #[test]
    fn move_into_an_empty_column_starts_at_gap() {
        let mut cards = board();
        apply_move(&mut cards, "b", TaskStatus::InReview, None);
        let b = &cards[3].task;
        assert_eq!((b.status, b.position), (TaskStatus::InReview, GAP));
    }

    #[test]
    fn move_before_takes_the_midpoint_with_the_predecessor() {
        let mut cards = board();
        apply_move(&mut cards, "c", TaskStatus::Todo, Some("b"));
        assert_eq!(ids(&cards, TaskStatus::Todo), ["a", "c", "b"]);
        assert_eq!(cards[0].task.position, 1536.0);

        apply_move(&mut cards, "x", TaskStatus::Todo, Some("a"));
        assert_eq!(ids(&cards, TaskStatus::Todo), ["x", "a", "c", "b"]);
        assert_eq!(cards[1].task.position, 512.0);
    }

    #[test]
    fn a_tiny_gap_renumbers_the_column() {
        let mut cards = vec![
            card("a", TaskStatus::Todo, 1.0),
            card("b", TaskStatus::Todo, 1.0 + 1e-7),
            card("m", TaskStatus::Done, 1.0),
        ];
        apply_move(&mut cards, "m", TaskStatus::Todo, Some("b"));
        assert_eq!(ids(&cards, TaskStatus::Todo), ["a", "m", "b"]);
        let positions: Vec<f64> = column(&cards, TaskStatus::Todo)
            .iter()
            .map(|c| c.task.position)
            .collect();
        assert_eq!(positions, [1024.0, 2048.0, 3072.0]);
    }

    #[test]
    fn unknown_ids_change_nothing() {
        let mut cards = board();
        assert!(!move_to(&mut cards, "nope", TaskStatus::Done, None));
        // `before_id` must be in the destination column, and is never the moved task.
        assert!(!move_to(&mut cards, "a", TaskStatus::Done, Some("b")));
        assert!(!move_to(&mut cards, "a", TaskStatus::Todo, Some("a")));
        assert_eq!(cards, board());
    }

    #[test]
    fn columns_are_per_project() {
        let mut tasks = vec![
            task("a", "p1", TaskStatus::Todo, 1024.0),
            task("z", "p2", TaskStatus::Todo, 9000.0),
            task("b", "p1", TaskStatus::Done, 1024.0),
        ];
        assert!(move_to(&mut tasks, "b", TaskStatus::Todo, None));
        assert_eq!(tasks[2].position, 2048.0);
        assert!(!move_to(&mut tasks, "b", TaskStatus::Todo, Some("z")));
    }

    #[test]
    fn random_moves_keep_a_strict_order() {
        let mut cards: Vec<TaskCard> = (0..12)
            .map(|i| card(&format!("t{i}"), COLS[i % 3], (i / 3 + 1) as f64 * GAP))
            .collect();
        // Reference model: the ids of each column, in order.
        let mut model: Vec<Vec<String>> = COLS.iter().map(|&s| ids(&cards, s)).collect();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64; // xorshift64, fixed seed
        let mut rand = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..500 {
            let id = format!("t{}", rand(12));
            let dest = rand(3);
            for col in &mut model {
                col.retain(|x| *x != id);
            }
            let at = rand(model[dest].len() + 1);
            let before = model[dest].get(at).cloned();
            model[dest].insert(at, id.clone());
            apply_move(&mut cards, &id, COLS[dest], before.as_deref());
            for (k, &s) in COLS.iter().enumerate() {
                assert_eq!(ids(&cards, s), model[k]);
                let col = column(&cards, s);
                assert!(
                    col.windows(2)
                        .all(|w| w[0].task.position < w[1].task.position)
                );
            }
        }
    }

    #[test]
    fn drop_before_skips_the_dragged_card_and_detects_no_ops() {
        let col = column(&board(), TaskStatus::Todo); // a b c
        let before = |index, dragged| drop_before(&col, index, dragged);
        // Right above or below itself: order unchanged.
        assert_eq!(before(1, "b"), None);
        assert_eq!(before(2, "b"), None);
        assert_eq!(before(3, "c"), None);
        assert_eq!(before(1, "a"), None);
        // Real moves inside the column.
        assert_eq!(before(0, "b"), Some(Some("a".into())));
        assert_eq!(before(3, "b"), Some(None));
        assert_eq!(before(0, "c"), Some(Some("a".into())));
        assert_eq!(before(2, "a"), Some(Some("c".into())));
        // From another column: any index, clamped at the end.
        assert_eq!(before(1, "x"), Some(Some("b".into())));
        assert_eq!(before(usize::MAX, "x"), Some(None));
        assert_eq!(drop_before(&[], 0, "x"), Some(None));
    }
}
