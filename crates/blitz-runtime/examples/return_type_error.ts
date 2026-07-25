// SPDX-License-Identifier: MPL-2.0
function invalidReturn(): number {
  return "not a number";
}

console.log(invalidReturn());
