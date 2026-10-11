const invalidArgument = (name, expected, value) => {
  const received = value === undefined ? "undefined" : value === null ? "null" : typeof value;
  const error = new TypeError(`The "${name}" property must be ${expected}. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const categoriesFrom = options => {
  if (options === null || typeof options !== "object") invalidArgument("options", "an object", options);
  const categories = options.categories;
  if (!Array.isArray(categories)) invalidArgument("options.categories", "an instance of Array", categories);
  if (categories.length === 0) {
    const error = new Error("At least one category is required");
    error.code = "ERR_TRACE_EVENTS_CATEGORY_REQUIRED";
    throw error;
  }
  for (const category of categories) {
    if (typeof category !== "string") invalidArgument("options.categories", "an array of strings", category);
  }
  return categories.join(",");
};

class Tracing {
  constructor(options) {
    this.categories = categoriesFrom(options);
    Object.defineProperty(this, "enabled", {
      value: false,
      enumerable: true,
      writable: false,
    });
  }

  enable() {
    const error = new Error("node:trace_events.enable is not implemented because Blitz has no trace sink");
    error.code = "ERR_NOT_IMPLEMENTED";
    throw error;
  }

  disable() {}
}

export const createTracing = options => new Tracing(options);
export const getEnabledCategories = () => "";

const traceEvents = Object.freeze({ createTracing, getEnabledCategories });
export default traceEvents;
