// SPDX-License-Identifier: MPL-2.0
var arr2 = [];
for (var i = 0; i < 100; i = i + 1) {
  arr2[i] = i;
}
var total2 = 0;
for (var j = 0; j < 1000; j = j + 1) {
  total2 = total2 + arr2.lastIndexOf(50);
}
