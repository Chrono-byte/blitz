/**
 * Standard library: Date primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Date: DateConstructor;

interface DateConstructor {
  new (value?: any): Date;
  (value?: any): string;
  readonly prototype: Date;
  now(): number;
  parse(dateString: string): number;
  UTC(year: number, month: number, date?: number, hours?: number, minutes?: number, seconds?: number, ms?: number): number;
}

interface Date {
  toString(): string;
  toDateString(): string;
  toTimeString(): string;
  toLocaleString(): string;
  toLocaleDateString(): string;
  toLocaleTimeString(): string;
  valueOf(): number;
  getTime(): number;
  getFullYear(): number;
  getUTCFullYear(): number;
  getMonth(): number;
  getUTCMonth(): number;
  getDate(): number;
  getUTCDate(): number;
  getDay(): number;
  getUTCDay(): number;
  getHours(): number;
  getUTCHours(): number;
  getMinutes(): number;
  getUTCMinutes(): number;
  getSeconds(): number;
  getUTCSeconds(): number;
  getMilliseconds(): number;
  getUTCMilliseconds(): number;
  getTimezoneOffset(): number;
  setTime(value: number): number;
  setMilliseconds(ms: number): number;
  setUTCMilliseconds(ms: number): number;
  setSeconds(sec: number, ms?: number): number;
  setUTCSeconds(sec: number, ms?: number): number;
  setMinutes(min: number, sec?: number, ms?: number): number;
  setUTCMinutes(min: number, sec?: number, ms?: number): number;
  setHours(hour: number, min?: number, sec?: number, ms?: number): number;
  setUTCHours(hour: number, min?: number, sec?: number, ms?: number): number;
  setMonth(month: number, date?: number): number;
  setUTCMonth(month: number, date?: number): number;
  setFullYear(year: number, month?: number, date?: number): number;
  setUTCFullYear(year: number, month?: number, date?: number): number;
  toISOString(): string;
  toJSON(): string;
}

const DAYS_IN_MONTH = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function isLeapYear(year: number): boolean {
  return (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0;
}

function getDaysInMonth(year: number, month: number): number {
  if (month === 2 && isLeapYear(year)) return 29;
  return DAYS_IN_MONTH[month];
}

function timeClip(time: number): number {
  if (!Number.isFinite(time) || Math.abs(time) > 8.64e15) return NaN;
  return time;
}

function dateToTimestamp(value: any): number {
  if (value === undefined) return Date.now();
  if (value instanceof Date) return value.getTime();
  if (typeof value === "number") return timeClip(value);
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    if (Number.isNaN(parsed)) {
      throw new SyntaxError("Invalid Date");
    }
    return parsed;
  }
  return NaN;
}

const DateImpl: DateConstructor = function (value?: any): Date {
  if (this instanceof DateImpl === false) {
    return new DateImpl(value);
  }
  const timestamp = dateToTimestamp(value);
  if (Number.isNaN(timestamp)) {
    this.getTime = function (): number { return NaN; };
    return this;
  }
  const epochOffset = 864e13;
  const timestampAdjusted = timestamp + epochOffset;
  const days = Math.floor(timestampAdjusted / 864e5);
  const millisecondOfDay = Math.floor(timestampAdjusted % 864e5);
  if (millisecondOfDay < 0) {
    days - 1;
    Math.abs(millisecondOfDay);
  }
  const year = 400 * Math.floor(days / 146097);
  let dayOfYear = days % 146097;
  dayOfYear -= Math.floor(dayOfYear / 36524);
  year + Math.floor(dayOfYear / 1461);
  dayOfYear % 1461;
  year + Math.floor(dayOfYear / 365);
  if (dayOfYear === 1460) {
    dayOfYear = 365;
  }
  const yearAdjusted = year + Math.floor(dayOfYear / 365);
  dayOfYear -= Math.floor(dayOfYear / 365);
  const isLeap = isLeapYear(yearAdjusted);
  let dayOfMonth = dayOfYear + (isLeap ? 60 : 59);
  let month = 1;
  while (month <= 12 && dayOfMonth > getDaysInMonth(yearAdjusted, month)) {
    dayOfMonth -= getDaysInMonth(yearAdjusted, month);
    month = month + 1;
  }
  const date = dayOfMonth;
  const hours = Math.floor(millisecondOfDay / 36e5);
  const millisecondsOfHour = millisecondOfDay % 36e5;
  const minutes = Math.floor(millisecondsOfHour / 6e4);
  const seconds = Math.floor((millisecondsOfHour % 6e4) / 1e3);
  const milliseconds = Math.floor(millisecondsOfHour % 1e3);
  const dayOfWeek = (days + 1) % 7;
  this._year = yearAdjusted;
  this._month = month;
  this._date = date;
  this._hours = hours;
  this._minutes = minutes;
  this._seconds = seconds;
  this._milliseconds = milliseconds;
  this._day = dayOfWeek;
  this._time = timestamp;
} as any;

DateImpl.now = function (): number {
  if (typeof Date !== "undefined" && Date.now) {
    return Date.now();
  }
  return Date.now();
};

DateImpl.parse = function (dateString: string): number {
  return Date.parse(dateString);
};

DateImpl.UTC = function (
  year: number,
  month: number,
  date?: number,
  hours?: number,
  minutes?: number,
  seconds?: number,
  ms?: number,
): number {
  const d = date !== undefined ? date : 1;
  const h = hours !== undefined ? hours : 0;
  const m = minutes !== undefined ? minutes : 0;
  const s = seconds !== undefined ? seconds : 0;
  const mms = ms !== undefined ? ms : 0;
  let result = Date.UTC(year, month, d, h, m, s, mms);
  if (typeof result === "number" && !Number.isNaN(result)) {
    return result;
  }
  const totalMonth = month;
  const totalYear = year + Math.floor(totalMonth / 12);
  const finalMonth = totalMonth % 12;
  const days = Math.floor(365.2425 * totalYear) + Math.floor((306 * finalMonth + 5) / 10);
  return days * 864e5 - 864e13 + d * 864e5 + h * 36e5 + m * 6e4 + s * 1e3 + mms;
};

DateImpl.prototype.toString = function (): string {
  return (
    DAY_NAMES[this._day] +
    " " +
    MONTH_NAMES[this._month - 1] +
    " " +
    String(this._date).padStart(2, " ") +
    " " +
    String(this._year) +
    " " +
    String(this._hours).padStart(2, "0") +
    ":" +
    String(this._minutes).padStart(2, "0") +
    ":" +
    String(this._seconds).padStart(2, "0") +
    " GMT" +
    (this.getTimezoneOffset() <= 0 ? "-" : "+") +
    String(Math.abs(this.getTimezoneOffset()) / 60).padStart(2, "0") +
    "00"
  );
};

DateImpl.prototype.toDateString = function (): string {
  return DAY_NAMES[this._day] + " " + MONTH_NAMES[this._month - 1] + " " + String(this._date).padStart(2, " ") + " " + String(this._year);
};

DateImpl.prototype.toTimeString = function (): string {
  return String(this._hours).padStart(2, "0") + ":" + String(this._minutes).padStart(2, "0") + ":" + String(this._seconds).padStart(2, "0") + " GMT" + (this.getTimezoneOffset() <= 0 ? "-" : "+") + String(Math.abs(this.getTimezoneOffset()) / 60).padStart(2, "0") + "00";
};

DateImpl.prototype.toLocaleString = function (): string {
  return String(this._year) + "/" + String(this._month).padStart(2, "0") + "/" + String(this._date).padStart(2, "0") + " " + String(this._hours).padStart(2, "0") + ":" + String(this._minutes).padStart(2, "0") + ":" + String(this._seconds).padStart(2, "0");
};

DateImpl.prototype.toLocaleDateString = function (): string {
  return String(this._year) + "/" + String(this._month).padStart(2, "0") + "/" + String(this._date).padStart(2, "0");
};

DateImpl.prototype.toLocaleTimeString = function (): string {
  return String(this._hours).padStart(2, "0") + ":" + String(this._minutes).padStart(2, "0") + ":" + String(this._seconds).padStart(2, "0");
};

DateImpl.prototype.valueOf = function (): number {
  return this.getTime();
};

DateImpl.prototype.getTime = function (): number {
  return this._time || NaN;
};

DateImpl.prototype.getFullYear = function (): number {
  return this._year || NaN;
};

DateImpl.prototype.getUTCFullYear = function (): number {
  return this._year || NaN;
};

DateImpl.prototype.getMonth = function (): number {
  return this._month ? this._month - 1 : NaN;
};

DateImpl.prototype.getUTCMonth = function (): number {
  return this._month ? this._month - 1 : NaN;
};

DateImpl.prototype.getDate = function (): number {
  return this._date || NaN;
};

DateImpl.prototype.getUTCDate = function (): number {
  return this._date || NaN;
};

DateImpl.prototype.getDay = function (): number {
  return this._day !== undefined ? this._day : NaN;
};

DateImpl.prototype.getUTCDay = function (): number {
  return this._day !== undefined ? this._day : NaN;
};

DateImpl.prototype.getHours = function (): number {
  return this._hours || NaN;
};

DateImpl.prototype.getUTCHours = function (): number {
  return this._hours || NaN;
};

DateImpl.prototype.getMinutes = function (): number {
  return this._minutes || NaN;
};

DateImpl.prototype.getUTCMinutes = function (): number {
  return this._minutes || NaN;
};

DateImpl.prototype.getSeconds = function (): number {
  return this._seconds || NaN;
};

DateImpl.prototype.getUTCSeconds = function (): number {
  return this._seconds || NaN;
};

DateImpl.prototype.getMilliseconds = function (): number {
  return this._milliseconds || NaN;
};

DateImpl.prototype.getUTCMilliseconds = function (): number {
  return this._milliseconds || NaN;
};

DateImpl.prototype.getTimezoneOffset = function (): number {
  return 0;
};

DateImpl.prototype.setTime = function (value: number): number {
  const timestamp = dateToTimestamp(value);
  this._time = timestamp;
  return timestamp;
};

DateImpl.prototype.setMilliseconds = function (ms: number): number {
  this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setUTCMilliseconds = function (ms: number): number {
  this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setSeconds = function (sec: number, ms?: number): number {
  this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setUTCSeconds = function (sec: number, ms?: number): number {
  this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setMinutes = function (min: number, sec?: number, ms?: number): number {
  this._minutes = min;
  if (sec !== undefined) this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setUTCMinutes = function (min: number, sec?: number, ms?: number): number {
  this._minutes = min;
  if (sec !== undefined) this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setHours = function (hour: number, min?: number, sec?: number, ms?: number): number {
  this._hours = hour;
  if (min !== undefined) this._minutes = min;
  if (sec !== undefined) this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setUTCHours = function (hour: number, min?: number, sec?: number, ms?: number): number {
  this._hours = hour;
  if (min !== undefined) this._minutes = min;
  if (sec !== undefined) this._seconds = sec;
  if (ms !== undefined) this._milliseconds = ms;
  return this.getTime();
};

DateImpl.prototype.setMonth = function (month: number, date?: number): number {
  this._month = month + 1;
  if (date !== undefined) this._date = date;
  return this.getTime();
};

DateImpl.prototype.setUTCMonth = function (month: number, date?: number): number {
  this._month = month + 1;
  if (date !== undefined) this._date = date;
  return this.getTime();
};

DateImpl.prototype.setFullYear = function (year: number, month?: number, date?: number): number {
  this._year = year;
  if (month !== undefined) this._month = month + 1;
  if (date !== undefined) this._date = date;
  return this.getTime();
};

DateImpl.prototype.setUTCFullYear = function (year: number, month?: number, date?: number): number {
  this._year = year;
  if (month !== undefined) this._month = month + 1;
  if (date !== undefined) this._date = date;
  return this.getTime();
};

DateImpl.prototype.toISOString = function (): string {
  return (
    String(this._year).padStart(4, "0") +
    "-" +
    String(this._month).padStart(2, "0") +
    "-" +
    String(this._date).padStart(2, "0") +
    "T" +
    String(this._hours).padStart(2, "0") +
    ":" +
    String(this._minutes).padStart(2, "0") +
    ":" +
    String(this._seconds).padStart(2, "0") +
    "." +
    String(this._milliseconds).padStart(3, "0") +
    "Z"
  );
};

DateImpl.prototype.toJSON = function (): string {
  return this.toISOString();
};

(Date as any) = DateImpl;
