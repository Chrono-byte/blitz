// SPDX-License-Identifier: MPL-2.0

use std::cmp::Ordering;

use crate::RuntimeError;

/// Stable merge sort. Comparator errors stop all further comparisons immediately.
pub(crate) fn sort_values<T: Clone>(
  values: &mut [T],
  mut compare: impl FnMut(&T, &T) -> Result<Ordering, RuntimeError>,
) -> Result<(), RuntimeError> {
  fn merge_sort<T: Clone>(
    values: &mut [T],
    scratch: &mut [T],
    compare: &mut impl FnMut(&T, &T) -> Result<Ordering, RuntimeError>,
  ) -> Result<(), RuntimeError> {
    if values.len() < 2 {
      return Ok(());
    }
    let middle = values.len() / 2;
    let (left, right) = values.split_at_mut(middle);
    let (left_scratch, right_scratch) = scratch.split_at_mut(middle);
    merge_sort(left, left_scratch, compare)?;
    merge_sort(right, right_scratch, compare)?;
    let mut left_index = 0;
    let mut right_index = 0;
    for destination in scratch.iter_mut() {
      let take_left = right_index == right.len()
        || (left_index < left.len()
          && compare(&left[left_index], &right[right_index])? != Ordering::Greater);
      if take_left {
        *destination = left[left_index].clone();
        left_index += 1;
      } else {
        *destination = right[right_index].clone();
        right_index += 1;
      }
    }
    values.clone_from_slice(scratch);
    Ok(())
  }

  if values.len() < 2 {
    return Ok(());
  }
  let mut scratch = values.to_vec();
  merge_sort(values, &mut scratch, &mut compare)
}
