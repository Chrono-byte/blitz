// SPDX-License-Identifier: MPL-2.0
var sum = 0;
for (var i = 0; i < 50000; i = i + 1) {
  sum = sum + (i % 2 === 0 ? i : -i);
}
