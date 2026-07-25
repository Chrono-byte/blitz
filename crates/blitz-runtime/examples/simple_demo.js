// SPDX-License-Identifier: MPL-2.0
const people = [
  { name: "Ada", language: "JavaScript" },
  { name: "Grace", language: "COBOL" },
];

function introduction({ name, language }) {
  return `${name} works with ${language}.`;
}

for (const person of people) {
  console.log(introduction(person));
}

const squares = [1, 2, 3, 4].map((value) => value * value);
console.log("squares:", squares);

try {
  throw new Error("example error");
} catch (error) {
  console.log("caught:", error.message);
}
