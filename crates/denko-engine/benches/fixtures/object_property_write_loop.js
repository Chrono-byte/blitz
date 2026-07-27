// SPDX-License-Identifier: MPL-2.0
var obj = {};
for (var i = 0; i < 5000; i = i + 1) {
  obj["key" + i] = i;
}
