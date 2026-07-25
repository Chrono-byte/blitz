// SPDX-License-Identifier: MPL-2.0
var total = 0;
var s = "héllo world";
for (var i = 0; i < 30000; i = i + 1) {
  total = total + s.indexOf("world");
  total = total + s.charCodeAt(1);
  total = total + s.slice(1, 4).length;
}
