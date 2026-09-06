// SPDX-License-Identifier: MPL-2.0
var values = new Float64Array(2048);
for (var i = 0; i < values.length; i++) values[i] = (i * 7919) % 2048;
var sorted = values.toSorted();
var checksum = 0;
for (var i = 0; i < sorted.length; i++) {
  if (sorted[i] !== i) throw new Error("typed array sort order changed");
  checksum += sorted[i];
}
