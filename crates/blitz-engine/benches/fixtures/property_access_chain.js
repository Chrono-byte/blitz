// SPDX-License-Identifier: MPL-2.0
var obj = { a: { b: { c: { d: 42 } } } };
var sum = 0;
for (var i = 0; i < 50000; i = i + 1) {
  sum = sum + obj.a.b.c.d;
}
