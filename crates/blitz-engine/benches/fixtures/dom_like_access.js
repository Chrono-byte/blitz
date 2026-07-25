// SPDX-License-Identifier: MPL-2.0
var dom = {
  children: [
    { id: 1, className: "active", innerHTML: "test1" },
    { id: 2, className: "inactive", innerHTML: "test2" },
  ],
  addEventListener: function () {},
  removeEventListener: function () {},
};
var sum = 0;
for (var i = 0; i < 10000; i = i + 1) {
  sum = sum + dom.children[0].id;
  sum = sum + dom.children[1].id;
}
