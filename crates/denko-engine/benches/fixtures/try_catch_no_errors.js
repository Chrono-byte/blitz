// SPDX-License-Identifier: MPL-2.0
var sum = 0;
for (var i = 0; i < 10000; i = i + 1) {
  try {
    sum = sum + i;
  } catch (e) {
    // never hit
  }
}
