// SPDX-License-Identifier: MPL-2.0
var arr = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
var evens = [];
for (var i = 0; i < 1000; i = i + 1) {
  evens = arr.filter(function (v) {
    return v % 2 === 0;
  });
}
