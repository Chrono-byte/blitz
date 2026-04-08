var obj = {
  getValue: function () {
    return 42;
  },
  add: function (n) {
    return this.getValue() + n;
  },
  multiply: function (n) {
    return this.add(10) * n;
  },
};
var sum = 0;
for (var i = 0; i < 10000; i = i + 1) {
  sum = sum + obj.multiply(2);
}
