//! Moving the selection in a list of `len` entries and keeping it in view.
//! Every move stops at the ends of the list; page sizes and wheel steps are
//! the caller's.

/// The last of `len` entries; 0 for an empty list.
pub fn last(len: usize) -> usize {
    len.saturating_sub(1)
}

/// `selected`, or the last entry if it is beyond.
pub fn clamp(selected: usize, len: usize) -> usize {
    selected.min(last(len))
}

/// `selected` moved `by` entries up.
pub fn up(selected: usize, by: usize) -> usize {
    selected.saturating_sub(by)
}

/// `selected` moved `by` entries down.
pub fn down(selected: usize, by: usize, len: usize) -> usize {
    clamp(selected.saturating_add(by), len)
}

/// The scroll offset that keeps `index` within `height` (at least 1) rows.
pub fn keep_visible(scroll: usize, index: usize, height: usize) -> usize {
    if index < scroll {
        index
    } else if index >= scroll + height {
        index + 1 - height
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_stop_at_the_ends() {
        assert_eq!(up(2, 1), 1);
        assert_eq!(up(2, 10), 0);
        assert_eq!(down(2, 1, 5), 3);
        assert_eq!(down(2, 10, 5), 4);
        assert_eq!(clamp(7, 5), 4);
        // An empty list has the entry 0 only.
        assert_eq!((last(0), clamp(3, 0), down(0, 1, 0)), (0, 0, 0));
    }

    #[test]
    fn scrolling_keeps_the_index_in_view() {
        assert_eq!(keep_visible(5, 3, 4), 3);
        assert_eq!(keep_visible(5, 8, 4), 5);
        assert_eq!(keep_visible(5, 9, 4), 6);
    }
}
