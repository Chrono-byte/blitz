/**
 * Standard library: JSON object.
 * Written in TypeScript, runs on the Rust VM.
 */

interface JSON {
  parse(text: string, reviver?: (key: string, value: any) => any): any;
  stringify(value: any, replacer?: (key: string, value: any) => any, space?: string | number): string;
}

const JSONImpl: JSON = {
  parse(text: string, reviver?: (key: string, value: any) => any): any {
    if (typeof text !== "string") {
      throw new TypeError("JSON.parse requires a string argument");
    }
    const parsed = JSON.parse(text);
    if (reviver !== undefined && typeof reviver === "function") {
      return walk(parsed, reviver);
    }
    return parsed;
  },
  stringify(value: any, replacer?: (key: string, value: any) => any, space?: string | number): string {
    if (replacer !== undefined && typeof replacer !== "function" && !Array.isArray(replacer)) {
      throw new TypeError("The replacer argument must be a function or an array");
    }
    let spaceStr: string;
    if (space === undefined) {
      spaceStr = "";
    } else if (typeof space === "number") {
      spaceStr = " ".repeat(Math.min(space, 10));
    } else if (typeof space === "string") {
      spaceStr = space.slice(0, 10);
    } else {
      spaceStr = "";
    }
    return JSON.stringify(value, replacer as any, spaceStr);
  },
};

function walk(value: any, reviver: (key: string, value: any) => any): any {
  if (value !== null && typeof value === "object") {
    if (Array.isArray(value)) {
      const result: any[] = [];
      let i = 0;
      while (i < value.length) {
        result[i] = walk(value[i], reviver);
        i = i + 1;
      }
      return result;
    }
    const result: { [key: string]: any } = {};
    const keys = Object.keys(value);
    let i = 0;
    while (i < keys.length) {
      const key = keys[i];
      result[key] = walk(value[key], reviver);
      i = i + 1;
    }
    return result;
  }
  return value;
}

const globalJSON = typeof JSON !== "undefined" ? JSON : {};
(globalJSON as any).parse = JSONImpl.parse;
(globalJSON as any).stringify = JSONImpl.stringify;
