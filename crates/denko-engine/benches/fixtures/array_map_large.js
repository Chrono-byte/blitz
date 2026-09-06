// SPDX-License-Identifier: MPL-2.0
var values = [];
for (var i = 0; i < 100000; i = i + 1) values.push(i);
var mapped = values.map(function (value) { return value * value; });
if (mapped[0] !== 0 || mapped[12345] !== 152399025) throw new Error("incorrect mapped element");
var checksum = mapped.length + mapped[99999];
