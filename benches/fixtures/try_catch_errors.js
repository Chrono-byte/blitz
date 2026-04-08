var sum = 0;
for (var i = 0; i < 1000; i = i + 1) {
  try {
    if (i === 500) throw new Error("test");
    sum = sum + i;
  } catch (e) {
    sum = sum - i;
  }
}
