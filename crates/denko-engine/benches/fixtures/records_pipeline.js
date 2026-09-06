// SPDX-License-Identifier: MPL-2.0
var records = [];
for (var i = 0; i < 2000; i++) {
  records.push({ group: "g" + (i % 10), amount: (i * 17) % 101, active: i % 3 !== 0 });
}
var selected = records.filter(function (record) { return record.active; });
var amounts = selected.map(function (record) { return { group: record.group, amount: record.amount * 2 }; });
var totals = {};
for (var i = 0; i < amounts.length; i++) {
  var record = amounts[i];
  totals[record.group] = (totals[record.group] || 0) + record.amount;
}
var ranked = Object.entries(JSON.parse(JSON.stringify(totals)));
ranked.sort(function (left, right) { return right[1] - left[1]; });
var checksum = 0;
for (var i = 0; i < ranked.length; i++) checksum += (i + 1) * ranked[i][1];
