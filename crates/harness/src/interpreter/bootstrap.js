(function (call, save, restoredJSON, dataObject) {
    "use strict";
    const parse = JSON.parse;
    const stringify = JSON.stringify;
    const descriptors = Object.getOwnPropertyDescriptors;
    const prototype = Object.getPrototypeOf;
    const ownKeys = Reflect.ownKeys;
    const isArray = Array.isArray;
    const objectPrototype = Object.prototype;
    const arrayPrototype = Array.prototype;
    const create = Object.create;
    const isFinite = Number.isFinite;
    const defineProperty = Object.defineProperty;
    const setPrototype = Object.setPrototypeOf;
    const SetConstructor = Set;
    const has = Function.prototype.call.bind(Set.prototype.has);
    const add = Function.prototype.call.bind(Set.prototype.add);
    const remove = Function.prototype.call.bind(Set.prototype.delete);
    const hasOwn = Function.prototype.call.bind(Object.prototype.hasOwnProperty);
    const number = Number;
    const string = String;
    const isInteger = Number.isInteger;
    const DataError = Error;
    const AsyncFunction = (async function () {}).constructor;
    // Copy only data properties. Never invoke getters/toJSON or silently drop values.
    function encode(value) {
        const seen = new SetConstructor();
        function copy(value) {
            if (value === null || typeof value === "string" || typeof value === "boolean") return value;
            if (typeof value === "number" && isFinite(value)) return value;
            if (typeof value !== "object") throw new DataError("value is not lossless JSON data");
            if (!dataObject(value)) throw new DataError("live objects are not JSON data");
            if (has(seen,value)) throw new DataError("cyclic checkpoint/result/arguments");
            const array = isArray(value);
            if (prototype(value) !== (array ? arrayPrototype : objectPrototype) && prototype(value) !== null)
                throw new DataError("live objects are not JSON data");
            add(seen,value);
            const props = descriptors(value);
            const result = array ? setPrototype([],null) : create(null);
            for (const key of ownKeys(props)) {
                if (array && key === "length") continue;
                if (typeof key !== "string" || !props[key].enumerable || !hasOwn(props[key],"value"))
                    throw new DataError("symbol, accessor or hidden property is not JSON data");
                if (array && ((string(number(key)) !== key || number(key) < 0 || !isInteger(number(key))) || number(key) >= value.length))
                    throw new DataError("array has non-index properties");
                // A plain descriptor literal inherits Object.prototype, so a poisoned
                // `get` accessor there would run script during encoding.
                const descriptor = create(null);
                descriptor.value = copy(props[key].value);
                descriptor.enumerable = true;
                descriptor.writable = true;
                descriptor.configurable = true;
                defineProperty(result, key, descriptor);
            }
            if (array && ownKeys(props).length !== value.length + 1) throw new DataError("sparse array is not lossless JSON data");
            remove(seen,value);
            return result;
        }
        return stringify(copy(value));
    }
    Object.defineProperty(globalThis, "host", {value:Object.freeze({
        call: async (name, args) => parse(await call(name, encode(args))),
        checkpoint: value => save(encode(value)),
    }), writable:false, configurable:false});
    globalThis.restored = parse(restoredJSON);
    return async code => {
        const value = await new AsyncFunction(code)();
        return encode(value === undefined ? null : value);
    };
})
