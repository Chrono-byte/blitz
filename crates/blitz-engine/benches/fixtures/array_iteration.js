// SPDX-License-Identifier: MPL-2.0
var data = [10, 20, 30, 40, 50];
var sum = 0;
for (var i = 0; i < 5000; i = i + 1) {
  for (var j = 0; j < data.length; j = j + 1) {
    sum = sum + data[j];
  }
}
