// SPDX-License-Identifier: MPL-2.0
var arr = [];
arr.length = 1000001;
arr[1000000] = 1;
arr[500000] = 1;
var total = 0;
for (var i = 0; i < 1000; i = i + 1) {
  total = total + arr.lastIndexOf(1);
}
