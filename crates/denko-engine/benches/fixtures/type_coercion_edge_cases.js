// SPDX-License-Identifier: MPL-2.0
var sum = 0;
for (var i = 0; i < 5000; i = i + 1) {
  sum = sum + ("5" + 3);
  sum = sum + (null + 5);
  sum = sum + (undefined + 5);
  sum = sum + (true + 5);
}
