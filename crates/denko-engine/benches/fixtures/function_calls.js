// SPDX-License-Identifier: MPL-2.0
function add(a, b) {
  return a + b;
}

var sum = 0;
for (var i = 0; i < 5000; i = i + 1) {
  sum = add(sum, i);
}
