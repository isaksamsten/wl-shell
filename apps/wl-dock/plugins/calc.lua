-- Calculator: a launcher source behind `=`. Type `=` in the empty launcher,
-- then an expression; the result is in front, Enter copies it.
--
-- Load it with a plugin table:
--
--   [plugin.calc]
--   precision = 12        # significant digits shown (1-17)
--   angle = "rad"         # or "deg": what sin, cos, tan take and asin... give
--   grouping = true       # 12,345,678 (copied without the commas)
--   extras = true         # hex and binary of integers, fractions, conversions
--   history = 20          # results kept for the empty query
--
-- What it understands:
--
--   2^10, 2**10, 7 // 2, 7 mod 2, 5!, 2(3 + 4), 2pi, -2^2, 2^3^2
--   sqrt 2, √2, sin(30°), log(8, 2), ln e, round(pi, 3), max(1, 5, 3)
--   15% of 80, 80 + 15%, 80 - 15%, 50%
--   0xff, 0b1010, 0o17, 1_000_000, 1.5e-3, 0xf0 & 0x3c, 1 << 10, 5 xor 3
--   255 in hex, 0xff in bin, 0.75 in frac, 1999 in roman, 12345 in sci
--   10 km in mi, 100°F to C, 1 GiB in MB, 90 min in h, 3 cups in ml,
--   65 mph in km/h; a quantity alone (5 ft) shows its usual conversions
--   x = 3 (Enter keeps it), then 2x; ans is the last result copied
--
-- An empty query lists the results copied before. Results only live as
-- long as the plugin runs.

local opts = { precision = 12, angle = "rad", grouping = true, extras = true, history = 20 }
local vars = {}
local history = {}
local ans = nil

local pi, huge = math.pi, math.huge
local ICON = "accessories-calculator"

-- Numbers ---------------------------------------------------------------

local function integral(v)
	return v == v and v ~= huge and v ~= -huge and v == math.floor(v)
end

-- `v` as plain text, for the clipboard: no grouping, `e` exponents.
local function raw(v)
	if v ~= v then
		return "NaN"
	end
	if v == huge then
		return "inf"
	end
	if v == -huge then
		return "-inf"
	end
	if v == 0 then
		return "0"
	end
	if integral(v) and math.abs(v) < 1e15 then
		return string.format("%d", v)
	end
	local s = string.format("%." .. opts.precision .. "g", v)
	local m, e = s:match("^(.-)(e.*)$")
	if not m then
		m, e = s, ""
	end
	if m:find("%.") then
		m = m:gsub("0+$", ""):gsub("%.$", "")
	end
	if e ~= "" then
		e = e:gsub("^e%+?(%-?)0*(%d)", "e%1%2")
	end
	return m .. e
end

local SUPER = {
	["0"] = "⁰",
	["1"] = "¹",
	["2"] = "²",
	["3"] = "³",
	["4"] = "⁴",
	["5"] = "⁵",
	["6"] = "⁶",
	["7"] = "⁷",
	["8"] = "⁸",
	["9"] = "⁹",
	["-"] = "⁻",
}

-- `v` for the eye: grouped digits, a real minus, `× 10ⁿ`, ∞.
local function pretty(v)
	local s = raw(v)
	if s == "inf" then
		return "∞"
	end
	if s == "-inf" then
		return "−∞"
	end
	if s == "NaN" then
		return "undefined"
	end
	local m, e = s:match("^(.-)e(.*)$")
	if not m then
		m, e = s, nil
	end
	local sign, int, rest = m:match("^(%-?)(%d+)(.*)$")
	if int and opts.grouping and #int > 4 then
		int = int:reverse():gsub("(%d%d%d)", "%1,"):reverse():gsub("^,", "")
	end
	m = (sign == "-" and "−" or "") .. (int or "") .. (rest or "")
	if e then
		m = m .. " × 10" .. e:gsub(".", SUPER)
	end
	return m
end

local function hex(v)
	local i = math.tointeger(v)
	if not i then
		return nil
	end
	return (i < 0 and "-" or "") .. string.format("0x%X", math.abs(i))
end

local function oct(v)
	local i = math.tointeger(v)
	if not i then
		return nil
	end
	return (i < 0 and "-" or "") .. string.format("0o%o", math.abs(i))
end

-- Binary, and the same in groups of four for the eye.
local function bin(v)
	local i = math.tointeger(v)
	if not i then
		return nil
	end
	local n, digits = math.abs(i), {}
	repeat
		table.insert(digits, 1, tostring(n % 2))
		n = n // 2
	until n == 0
	local s = table.concat(digits)
	local grouped = s:reverse():gsub("(%d%d%d%d)", "%1 "):reverse():gsub("^ ", "")
	local sign = i < 0 and "-" or ""
	return sign .. "0b" .. s, sign .. "0b" .. grouped
end

local function roman(v)
	local i = math.tointeger(v)
	if not i or i < 1 or i > 3999 then
		return nil
	end
	local out = {}
	for _, p in ipairs({
		{ 1000, "M" },
		{ 900, "CM" },
		{ 500, "D" },
		{ 400, "CD" },
		{ 100, "C" },
		{ 90, "XC" },
		{ 50, "L" },
		{ 40, "XL" },
		{ 10, "X" },
		{ 9, "IX" },
		{ 5, "V" },
		{ 4, "IV" },
		{ 1, "I" },
	}) do
		while i >= p[1] do
			out[#out + 1] = p[2]
			i = i - p[1]
		end
	end
	return table.concat(out)
end

-- `x` as a fraction with a denominator up to `max`, if one is exact.
local function fraction(x, max)
	if not (x == x and math.abs(x) < huge) or integral(x) then
		return nil
	end
	local sign = x < 0 and "-" or ""
	x = math.abs(x)
	local h0, h1, k0, k1, y = 0, 1, 1, 0, x
	for _ = 1, 40 do
		local a = math.floor(y)
		local h2, k2 = a * h1 + h0, a * k1 + k0
		if k2 > max then
			break
		end
		h0, h1, k0, k1 = h1, h2, k1, k2
		if math.abs(x - h1 / k1) <= 1e-12 * math.max(1, x) then
			return string.format("%s%d/%d", sign, h1, k1)
		end
		local f = y - a
		if f < 1e-15 then
			break
		end
		y = 1 / f
	end
	return nil
end

-- Γ(x), by Lanczos, for factorials of non-integers.
local LANCZOS = {
	676.5203681218851,
	-1259.1392167224028,
	771.32342877765313,
	-176.61503916999185,
	12.507343278686905,
	-0.13857109526572012,
	9.9843695780195716e-6,
	1.5056327351493116e-7,
}
local function gamma(x)
	if x < 0.5 then
		return pi / (math.sin(pi * x) * gamma(1 - x))
	end
	x = x - 1
	local a, t = 0.99999999999980993, x + 7.5
	for i, c in ipairs(LANCZOS) do
		a = a + c / (x + i)
	end
	return math.sqrt(2 * pi) * t ^ (x + 0.5) * math.exp(-t) * a
end

local function factorial(n)
	if integral(n) then
		if n < 0 then
			error({ msg = "no factorial of a negative integer" }, 0)
		end
		if n > 170 then
			return huge
		end
		local r = 1.0
		for i = 2, n do
			r = r * i
		end
		return r
	end
	return gamma(n + 1)
end

local function gcd(a, b)
	a, b = math.abs(a), math.abs(b)
	while b > 0 do
		a, b = b, a % b
	end
	return a
end

-- Units -----------------------------------------------------------------

-- By name; `exact` holds the names that only count with their case
-- (b is a bit, B a byte), `loose` the rest by their lower case.
local exact, loose, nice = {}, {}, {}

-- A unit of dimension `dim`: `f` of the base unit, plus `o` (temperatures).
local function unit(dim, sym, f, names, o, only_exact)
	local u = { dim = dim, sym = sym, f = f, o = o or 0 }
	for _, n in ipairs(names) do
		exact[n] = exact[n] or u
		if not only_exact then
			local l = n:lower()
			loose[l] = loose[l] or u
		end
	end
	return u
end

local function units(dim, list, favourites)
	for _, u in ipairs(list) do
		unit(dim, table.unpack(u))
	end
	nice[dim] = favourites
end

units("length", {
	{ "m", 1, { "m", "meter", "meters", "metre", "metres" } },
	{ "km", 1e3, { "km", "kilometer", "kilometers", "kilometre", "kilometres" } },
	{ "cm", 1e-2, { "cm", "centimeter", "centimeters", "centimetre", "centimetres" } },
	{ "mm", 1e-3, { "mm", "millimeter", "millimeters", "millimetre", "millimetres" } },
	{ "µm", 1e-6, { "um", "micrometer", "micrometers", "micron", "microns" } },
	{ "nm", 1e-9, { "nm", "nanometer", "nanometers" } },
	{ "mi", 1609.344, { "mi", "mile", "miles" } },
	{ "yd", 0.9144, { "yd", "yard", "yards" } },
	{ "ft", 0.3048, { "ft", "foot", "feet" } },
	{ "in", 0.0254, { "in", "inch", "inches" } },
	{ "nmi", 1852, { "nmi", "nauticalmile", "nauticalmiles" } },
	{ "au", 1.495978707e11, { "au" } },
	{ "ly", 9.4607304725808e15, { "ly", "lightyear", "lightyears" } },
}, { "m", "km", "cm", "mm", "mi", "ft", "in", "yd" })

units("mass", {
	{ "kg", 1, { "kg", "kilogram", "kilograms", "kilo", "kilos" } },
	{ "g", 1e-3, { "g", "gram", "grams" } },
	{ "mg", 1e-6, { "mg", "milligram", "milligrams" } },
	{ "t", 1e3, { "t", "tonne", "tonnes" } },
	{ "lb", 0.45359237, { "lb", "lbs", "pound", "pounds" } },
	{ "oz", 0.028349523125, { "oz", "ounce", "ounces" } },
	{ "st", 6.35029318, { "st", "stone", "stones" } },
}, { "kg", "g", "lb", "oz", "t", "st" })

units("time", {
	{ "s", 1, { "s", "sec", "secs", "second", "seconds" } },
	{ "ms", 1e-3, { "ms", "millisecond", "milliseconds" } },
	{ "µs", 1e-6, { "us", "microsecond", "microseconds" } },
	{ "ns", 1e-9, { "ns", "nanosecond", "nanoseconds" } },
	{ "min", 60, { "min", "mins", "minute", "minutes" } },
	{ "h", 3600, { "h", "hr", "hrs", "hour", "hours" } },
	{ "d", 86400, { "d", "day", "days" } },
	{ "wk", 604800, { "wk", "week", "weeks" } },
	{ "mo", 2629746, { "mo", "month", "months" } },
	{ "yr", 31556952, { "yr", "yrs", "year", "years" } },
}, { "s", "min", "h", "d", "wk", "yr", "ms" })

units("temperature", {
	{ "°C", 1, { "C", "celsius", "degC" }, 273.15 },
	{ "°F", 5 / 9, { "F", "fahrenheit", "degF" }, 273.15 - 32 * 5 / 9 },
	{ "K", 1, { "K", "kelvin", "kelvins" } },
}, { "°C", "°F", "K" })

units("data", {
	{ "B", 1, { "B", "byte", "bytes" } },
	{ "kB", 1e3, { "kB", "KB", "kilobyte", "kilobytes" } },
	{ "MB", 1e6, { "MB", "megabyte", "megabytes" } },
	{ "GB", 1e9, { "GB", "gigabyte", "gigabytes" } },
	{ "TB", 1e12, { "TB", "terabyte", "terabytes" } },
	{ "PB", 1e15, { "PB", "petabyte", "petabytes" } },
	{ "KiB", 2 ^ 10, { "KiB", "kibibyte", "kibibytes" } },
	{ "MiB", 2 ^ 20, { "MiB", "mebibyte", "mebibytes" } },
	{ "GiB", 2 ^ 30, { "GiB", "gibibyte", "gibibytes" } },
	{ "TiB", 2 ^ 40, { "TiB", "tebibyte", "tebibytes" } },
	{ "bit", 1 / 8, { "bit", "bits" } },
	{ "kbit", 125, { "kbit", "kilobit", "kilobits" } },
	{ "Mbit", 125e3, { "Mbit", "megabit", "megabits" } },
	{ "Gbit", 125e6, { "Gbit", "gigabit", "gigabits" } },
}, { "B", "kB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB" })
-- Bits by their symbols, only with their case.
unit("data", "bit", 1 / 8, { "b" }, nil, true)
unit("data", "kbit", 125, { "Kb" }, nil, true)
unit("data", "Mbit", 125e3, { "Mb" }, nil, true)
unit("data", "Gbit", 125e6, { "Gb" }, nil, true)

units("speed", {
	{ "m/s", 1, { "m/s", "mps" } },
	{ "km/h", 1 / 3.6, { "km/h", "kmh", "kph" } },
	{ "mph", 0.44704, { "mph", "mi/h" } },
	{ "kn", 1852 / 3600, { "kn", "kt", "knot", "knots" } },
	{ "ft/s", 0.3048, { "ft/s", "fps" } },
}, { "km/h", "m/s", "mph", "kn" })

units("area", {
	{ "m²", 1, { "m2", "sqm" } },
	{ "km²", 1e6, { "km2", "sqkm" } },
	{ "cm²", 1e-4, { "cm2" } },
	{ "ha", 1e4, { "ha", "hectare", "hectares" } },
	{ "acre", 4046.8564224, { "acre", "acres", "ac" } },
	{ "ft²", 0.09290304, { "ft2", "sqft" } },
	{ "mi²", 2589988.110336, { "mi2", "sqmi" } },
}, { "m²", "km²", "ha", "acre", "ft²" })

units("volume", {
	{ "l", 1, { "l", "L", "liter", "liters", "litre", "litres" } },
	{ "ml", 1e-3, { "ml", "mL", "milliliter", "milliliters", "millilitre", "millilitres" } },
	{ "cl", 1e-2, { "cl", "cL" } },
	{ "dl", 1e-1, { "dl", "dL" } },
	{ "m³", 1e3, { "m3" } },
	{ "cm³", 1e-3, { "cm3", "cc" } },
	{ "gal", 3.785411784, { "gal", "gallon", "gallons" } },
	{ "qt", 0.946352946, { "qt", "quart", "quarts" } },
	{ "pt", 0.473176473, { "pt", "pint", "pints" } },
	{ "cup", 0.2365882365, { "cup", "cups" } },
	{ "fl oz", 0.0295735295625, { "floz", "fl_oz" } },
	{ "tbsp", 0.01478676478125, { "tbsp", "tablespoon", "tablespoons" } },
	{ "tsp", 0.00492892159375, { "tsp", "teaspoon", "teaspoons" } },
}, { "l", "ml", "gal", "cup", "fl oz" })

units("angle", {
	{ "rad", 1, { "rad", "radian", "radians" } },
	{ "°", pi / 180, { "deg", "degree", "degrees" } },
	{ "turn", 2 * pi, { "turn", "turns", "rev" } },
	{ "grad", pi / 200, { "grad", "gon" } },
}, { "°", "rad", "turn" })

units("energy", {
	{ "J", 1, { "J", "joule", "joules" } },
	{ "kJ", 1e3, { "kJ" } },
	{ "cal", 4.184, { "cal", "calorie", "calories" } },
	{ "kcal", 4184, { "kcal" } },
	{ "Wh", 3600, { "Wh" } },
	{ "kWh", 3.6e6, { "kWh" } },
	{ "eV", 1.602176634e-19, { "eV" } },
}, { "J", "kJ", "kcal", "kWh" })

units("pressure", {
	{ "Pa", 1, { "Pa", "pascal", "pascals" } },
	{ "kPa", 1e3, { "kPa" } },
	{ "bar", 1e5, { "bar" } },
	{ "mbar", 100, { "mbar", "hPa" } },
	{ "psi", 6894.757293168, { "psi" } },
	{ "atm", 101325, { "atm" } },
	{ "mmHg", 133.322387415, { "mmHg" } },
}, { "kPa", "bar", "psi", "atm" })

units("power", {
	{ "W", 1, { "W", "watt", "watts" } },
	{ "kW", 1e3, { "kW" } },
	{ "MW", 1e6, { "MW" } },
	{ "hp", 745.69987158227, { "hp", "horsepower" } },
}, { "W", "kW", "hp" })

-- The units by their symbols, for the favourites.
local by_sym = {}
for _, t in ipairs({ exact, loose }) do
	for _, u in pairs(t) do
		by_sym[u.dim .. ":" .. u.sym] = u
	end
end

local function find_unit(name)
	return exact[name] or loose[name:lower()]
end

local function convert(v, from, to)
	return ((v * from.f + from.o) - to.o) / to.f
end

-- What a result can be shown as: `in hex` and so on.
local FORMATS = {
	hex = "hex",
	hexadecimal = "hex",
	bin = "bin",
	binary = "bin",
	oct = "oct",
	octal = "oct",
	dec = "dec",
	decimal = "dec",
	sci = "sci",
	scientific = "sci",
	frac = "frac",
	fraction = "frac",
	roman = "roman",
}

-- Expressions -----------------------------------------------------------

-- Words with a meaning of their own: never a name.
local KEYWORDS = { ["in"] = true, to = true, as = true, of = true, mod = true, xor = true }
local CONVERT = { ["in"] = true, to = true, as = true }

local function fail(msg, incomplete)
	error({ msg = msg, incomplete = incomplete }, 0)
end

-- Angles in and out of the trigonometric functions.
local function angle_in(x)
	return opts.angle == "deg" and x * pi / 180 or x
end
local function angle_out(x)
	return opts.angle == "deg" and x * 180 / pi or x
end
local function snap(x)
	return math.abs(x) < 1e-15 and 0.0 or x
end

local function each(args, f)
	local r = args[1]
	for i = 2, #args do
		r = f(r, args[i])
	end
	return r
end

-- Functions: `fn` and how many arguments they take.
local FUNCS = {
	sqrt = { math.sqrt, 1 },
	cbrt = {
		function(x)
			return x < 0 and -(-x) ^ (1 / 3) or x ^ (1 / 3)
		end,
		1,
	},
	abs = { math.abs, 1 },
	sign = {
		function(x)
			return x > 0 and 1 or x < 0 and -1 or 0
		end,
		1,
	},
	floor = { math.floor, 1 },
	ceil = { math.ceil, 1 },
	trunc = {
		function(x)
			return x < 0 and math.ceil(x) or math.floor(x)
		end,
		1,
	},
	round = {
		function(x, n)
			local m = 10 ^ math.floor(n or 0)
			return math.floor(x * m + 0.5) / m
		end,
		1,
		2,
	},
	exp = { math.exp, 1 },
	ln = { math.log, 1 },
	log = {
		function(x, b)
			return math.log(x, b or 10)
		end,
		1,
		2,
	},
	log2 = {
		function(x)
			return math.log(x, 2)
		end,
		1,
	},
	log10 = {
		function(x)
			return math.log(x, 10)
		end,
		1,
	},
	sin = {
		function(x)
			return snap(math.sin(angle_in(x)))
		end,
		1,
	},
	cos = {
		function(x)
			return snap(math.cos(angle_in(x)))
		end,
		1,
	},
	tan = {
		function(x)
			return snap(math.tan(angle_in(x)))
		end,
		1,
	},
	asin = {
		function(x)
			return angle_out(math.asin(x))
		end,
		1,
	},
	acos = {
		function(x)
			return angle_out(math.acos(x))
		end,
		1,
	},
	atan = {
		function(y, x)
			return angle_out(math.atan(y, x))
		end,
		1,
		2,
	},
	atan2 = {
		function(y, x)
			return angle_out(math.atan(y, x))
		end,
		2,
	},
	sinh = {
		function(x)
			return (math.exp(x) - math.exp(-x)) / 2
		end,
		1,
	},
	cosh = {
		function(x)
			return (math.exp(x) + math.exp(-x)) / 2
		end,
		1,
	},
	tanh = {
		function(x)
			local a, b = math.exp(x), math.exp(-x)
			return (a - b) / (a + b)
		end,
		1,
	},
	asinh = {
		function(x)
			return math.log(x + math.sqrt(x * x + 1))
		end,
		1,
	},
	acosh = {
		function(x)
			return math.log(x + math.sqrt(x * x - 1))
		end,
		1,
	},
	atanh = {
		function(x)
			return 0.5 * math.log((1 + x) / (1 - x))
		end,
		1,
	},
	hypot = {
		function(...)
			local s = 0
			for _, v in ipairs({ ... }) do
				s = s + v * v
			end
			return math.sqrt(s)
		end,
		1,
		huge,
	},
	min = {
		function(...)
			return each({ ... }, math.min)
		end,
		1,
		huge,
	},
	max = {
		function(...)
			return each({ ... }, math.max)
		end,
		1,
		huge,
	},
	sum = {
		function(...)
			return each({ ... }, function(a, b)
				return a + b
			end)
		end,
		1,
		huge,
	},
	avg = {
		function(...)
			return each({ ... }, function(a, b)
				return a + b
			end) / select("#", ...)
		end,
		1,
		huge,
	},
	gcd = {
		function(...)
			return each({ ... }, gcd)
		end,
		1,
		huge,
	},
	lcm = {
		function(...)
			return each({ ... }, function(a, b)
				return a == 0 and 0 or math.abs(a * b) / gcd(a, b)
			end)
		end,
		1,
		huge,
	},
	fact = { factorial, 1 },
	gamma = { gamma, 1 },
	ncr = {
		function(n, k)
			return factorial(n) / (factorial(k) * factorial(n - k))
		end,
		2,
	},
	npr = {
		function(n, k)
			return factorial(n) / factorial(n - k)
		end,
		2,
	},
	deg = {
		function(x)
			return x * 180 / pi
		end,
		1,
	},
	rad = {
		function(x)
			return x * pi / 180
		end,
		1,
	},
}
FUNCS.mean = FUNCS.avg
FUNCS.factorial = FUNCS.fact
FUNCS.choose = FUNCS.ncr
FUNCS.perm = FUNCS.npr
FUNCS.arcsin, FUNCS.arccos, FUNCS.arctan = FUNCS.asin, FUNCS.acos, FUNCS.atan

-- Constants. `deg` is one degree in the angle unit, so sin(30 deg) and
-- sin(30°) are right in either mode.
local function constant(name)
	if name == "pi" then
		return pi
	end
	if name == "tau" then
		return 2 * pi
	end
	if name == "e" then
		return math.exp(1)
	end
	if name == "phi" then
		return (1 + math.sqrt(5)) / 2
	end
	if name == "inf" or name == "infinity" then
		return huge
	end
	if name == "deg" then
		return opts.angle == "deg" and 1 or pi / 180
	end
	if name == "rad" then
		return opts.angle == "deg" and 180 / pi or 1
	end
	return nil
end

local function lookup(name)
	if vars[name] ~= nil then
		return vars[name]
	end
	if name == "ans" then
		if ans == nil then
			fail("no result copied yet")
		end
		return ans
	end
	return constant(name) or constant(name:lower())
end

-- Symbols people type, in the plain ASCII the tokens are made of.
local function normalize(s)
	s = s:gsub("×", "*"):gsub("·", "*"):gsub("÷", "/"):gsub("−", "-"):gsub("–", "-")
	s = s:gsub("π", " pi "):gsub("τ", " tau "):gsub("φ", " phi "):gsub("√", " sqrt ")
	s = s:gsub("µ", "u"):gsub("μ", "u")
	-- Square and cubic units (m², ft³) are names; other powers are powers.
	for sup, n in pairs({ ["²"] = "2", ["³"] = "3" }) do
		s = s:gsub("([%a_][%w_]*)" .. sup, function(w)
			return find_unit(w .. n) and w .. n or w .. "^" .. n
		end)
		s = s:gsub(sup, "^" .. n)
	end
	s = s:gsub("°%s*([CcFfKk])", " deg%1"):gsub("°", " deg ")
	return s
end

local function tokenize(s)
	local toks, i, n = {}, 1, #s
	local function push(t, v)
		toks[#toks + 1] = { t = t, v = v }
	end
	while i <= n do
		local c = s:sub(i, i)
		if c:match("%s") then
			i = i + 1
		else
			local a, b, base
			for _, p in ipairs({ { "^0[xX][%x_]+", 16 }, { "^0[bB][01_]+", 2 }, { "^0[oO][0-7_]+", 8 } }) do
				a, b = s:find(p[1], i)
				if a then
					base = p[2]
					break
				end
			end
			if a then
				local v = tonumber((s:sub(a + 2, b):gsub("_", "")), base)
				if not v then
					fail("bad number")
				end
				push("num", v + 0.0)
				i = b + 1
			else
				for _, p in ipairs({
					"^%d[%d_]*%.?[%d_]*[eE][%+%-]?%d+",
					"^%.%d[%d_]*[eE][%+%-]?%d+",
					"^%d[%d_]*%.?[%d_]*",
					"^%.%d[%d_]*",
				}) do
					a, b = s:find(p, i)
					if a then
						break
					end
				end
				if a then
					local v = tonumber((s:sub(a, b):gsub("_", "")))
					if not v then
						fail("bad number")
					end
					push("num", v + 0.0)
					i = b + 1
				else
					a, b = s:find("^[%a_][%w_]*", i)
					if a then
						push("id", s:sub(a, b))
						i = b + 1
					else
						local two = s:sub(i, i + 1)
						if two == "**" then
							push("op", "^")
							i = i + 2
						elseif two == "<<" or two == ">>" or two == "//" then
							push("op", two)
							i = i + 2
						elseif c:match("[%+%-%*/%%%^%(%),!=&|~]") then
							push("op", c)
							i = i + 1
						else
							fail("unexpected " .. c)
						end
					end
				end
			end
		end
	end
	return toks
end

local function isop(t, v)
	return t ~= nil and t.t == "op" and t.v == v
end
local function isid(t, v)
	return t ~= nil and t.t == "id" and t.v == v
end

local function int(v)
	local i = math.tointeger(v)
	if not i then
		fail("bit operations take integers")
	end
	return i
end

-- The value of the expression `toks`.
local function parse(toks)
	local p = 1
	local function peek()
		return toks[p]
	end
	local function take()
		p = p + 1
		return toks[p - 1]
	end
	local function expect(v)
		if isop(peek(), v) then
			return take()
		end
		fail("expected " .. v, peek() == nil)
	end
	-- Whether `t` starts an operand: a number, a name or `(`.
	local function operand(t)
		return t ~= nil and (t.t == "num" or (t.t == "id" and not KEYWORDS[t.v]) or isop(t, "("))
	end
	local expr, unary, power

	local function call(name, f, args)
		local fn, least, most = f[1], f[2], f[3] or f[2]
		if #args < least or #args > most then
			fail(name .. " takes " .. (least == most and least or least .. " or more") .. " arguments")
		end
		return fn(table.unpack(args)) + 0.0
	end

	local function primary()
		local t = peek()
		if t == nil then
			fail("incomplete", true)
		end
		if t.t == "num" then
			take()
			return t.v
		end
		if isop(t, "(") then
			take()
			local v = expr()
			expect(")")
			return v
		end
		if t.t == "id" and not KEYWORDS[t.v] then
			take()
			local f = FUNCS[t.v] or FUNCS[t.v:lower()]
			if f and isop(peek(), "(") then
				take()
				local args = {}
				if not isop(peek(), ")") then
					args[1] = expr()
					while isop(peek(), ",") do
						take()
						args[#args + 1] = expr()
					end
				end
				expect(")")
				return call(t.v, f, args)
			end
			local v = lookup(t.v)
			if v ~= nil then
				return v
			end
			-- `sqrt 2`, `sin 30 deg`: a function of what follows, up to
			-- the next operator (`sqrt 4 * 2` is 4).
			if f and operand(peek()) then
				local arg = power()
				while operand(peek()) do
					arg = arg * power()
				end
				return call(t.v, f, { arg })
			end
			fail("unknown name " .. t.v, p > #toks)
		end
		fail("unexpected " .. tostring(t.v), false)
	end

	-- `5!`, and `50%` (a percent unless an operand follows: that is `mod`).
	local function postfix()
		local v, pct = primary(), false
		while true do
			if isop(peek(), "!") then
				take()
				v, pct = factorial(v), false
			elseif isop(peek(), "%") and not operand(toks[p + 1]) then
				take()
				v, pct = v / 100, true
			else
				return v, pct
			end
		end
	end

	-- Right-associative, and tighter than a sign: -2^2 is -4.
	function power()
		local v, pct = postfix()
		if isop(peek(), "^") then
			take()
			return v ^ unary(), false
		end
		return v, pct
	end

	function unary()
		if isop(peek(), "-") then
			take()
			local v, pct = unary()
			return -v, pct
		end
		if isop(peek(), "+") then
			take()
			return unary()
		end
		if isop(peek(), "~") then
			take()
			return (~int((unary()))) + 0.0, false
		end
		return power()
	end

	-- `*`, `/`, `//`, `mod`, `of` (15% of 80), and 2pi, 2(3 + 4).
	local function term()
		local v, pct = unary()
		while true do
			local t = peek()
			if isop(t, "*") or isid(t, "of") then
				take()
				v, pct = v * unary(), false
			elseif isop(t, "/") then
				take()
				v, pct = v / unary(), false
			elseif isop(t, "//") then
				take()
				v, pct = math.floor(v / unary()), false
			elseif isop(t, "%") or isid(t, "mod") then
				take()
				local d = unary()
				v, pct = v - math.floor(v / d) * d, false
			elseif operand(t) then
				v, pct = v * unary(), false
			else
				return v, pct
			end
		end
	end

	-- `80 + 15%` is 80 plus 15% of it.
	local function additive()
		local v = term()
		while isop(peek(), "+") or isop(peek(), "-") do
			local plus = take().v == "+"
			local r, pct = term()
			if pct then
				r = v * r
			end
			v = plus and v + r or v - r
		end
		return v
	end

	local function shift()
		local v = additive()
		while isop(peek(), "<<") or isop(peek(), ">>") do
			local left = take().v == "<<"
			local a, b = int(v), int(additive())
			v = (left and a << b or a >> b) + 0.0
		end
		return v
	end

	local function band()
		local v = shift()
		while isop(peek(), "&") do
			take()
			v = (int(v) & int(shift())) + 0.0
		end
		return v
	end

	local function bxor()
		local v = band()
		while isid(peek(), "xor") do
			take()
			v = (int(v) ~ int(band())) + 0.0
		end
		return v
	end

	function expr()
		local v = bxor()
		while isop(peek(), "|") do
			take()
			v = (int(v) | int(bxor())) + 0.0
		end
		return v
	end

	local v = expr()
	if p <= #toks then
		fail("unexpected " .. tostring(toks[p].v), false)
	end
	return v
end

local function slice(t, i, j)
	return { table.unpack(t, i, j) }
end

-- A unit at the end of `toks` (`km`, `km/h`), unless it is a name in use.
local function strip_unit(toks)
	local n = #toks
	if n >= 4 and toks[n].t == "id" and isop(toks[n - 1], "/") and toks[n - 2].t == "id" then
		local u = find_unit(toks[n - 2].v .. "/" .. toks[n].v)
		if u then
			return slice(toks, 1, n - 3), u
		end
	end
	local last = toks[n]
	if n >= 2 and last.t == "id" and vars[last.v] == nil then
		local u = find_unit(last.v)
		-- `sin 30 deg` is a sine, not an angle.
		if u and u.dim == "angle" then
			for i = 1, n - 1 do
				if toks[i].t == "id" and (FUNCS[toks[i].v] or FUNCS[toks[i].v:lower()]) then
					u = nil
				end
			end
		end
		if u then
			return slice(toks, 1, n - 1), u
		end
	end
	return toks, nil
end

-- What `to` names: a unit or a format.
local function target(toks)
	if #toks == 3 and toks[1].t == "id" and isop(toks[2], "/") and toks[3].t == "id" then
		local u = find_unit(toks[1].v .. "/" .. toks[3].v)
		if u then
			return { unit = u }
		end
	end
	if #toks == 1 and toks[1].t == "id" then
		local name = toks[1].v
		if FORMATS[name:lower()] then
			return { format = FORMATS[name:lower()] }
		end
		local u = find_unit(name)
		if u then
			return { unit = u }
		end
		fail("unknown unit " .. name, true)
	end
	fail("expected a unit", true)
end

-- The query, understood: its value, and what to show it as.
local function evaluate(query)
	local toks = tokenize(normalize(query))
	if #toks == 0 then
		fail("empty", true)
	end
	local assign
	if #toks >= 3 and toks[1].t == "id" and isop(toks[2], "=") then
		local name = toks[1].v
		if KEYWORDS[name] or FUNCS[name] or constant(name) or name == "ans" then
			fail(name .. " is taken")
		end
		assign, toks = name, slice(toks, 3)
	end
	local to
	for k = #toks - 1, 2, -1 do
		if toks[k].t == "id" and CONVERT[toks[k].v] then
			to = target(slice(toks, k + 1))
			toks = slice(toks, 1, k - 1)
			break
		end
	end
	local from
	toks, from = strip_unit(toks)
	if #toks == 0 then
		fail("incomplete", true)
	end
	return { value = parse(toks), unit = from, to = to, assign = assign }
end

-- Results ---------------------------------------------------------------

local function row(id, title, value, subtitle, extra)
	local t = { id = id, title = title, value = value, subtitle = subtitle, icon = ICON }
	for k, v in pairs(extra or {}) do
		t[k] = v
	end
	-- JSON has no infinity: such a number isn't kept for `ans`.
	if t.number and not (math.abs(t.number) < huge) then
		t.number = nil
	end
	return t
end

-- How far `v` is from a comfortable size (1 to 999), for picking units.
local function awkward(v)
	if v == 0 then
		return 0
	end
	local m = math.log(math.abs(v), 10)
	if m >= 0 and m < 3 then
		return 0
	end
	return m < 0 and -m or m - 3
end

-- `v` of `from` in the dimension's favourite units, the most readable first.
local function conversions(v, from, limit)
	local out = {}
	for i, sym in ipairs(nice[from.dim]) do
		local u = by_sym[from.dim .. ":" .. sym]
		if u and u ~= from then
			local c = convert(v, from, u)
			local score = from.dim == "temperature" and 0 or awkward(c)
			out[#out + 1] = { u = u, v = c, score = score, i = i }
		end
	end
	table.sort(out, function(a, b)
		if a.score ~= b.score then
			return a.score < b.score
		end
		return a.i < b.i
	end)
	local rows = {}
	for i = 1, math.min(limit, #out) do
		local c = out[i]
		rows[#rows + 1] = row(
			"unit:" .. c.u.sym,
			pretty(c.v) .. " " .. c.u.sym,
			raw(c.v),
			pretty(v) .. " " .. from.sym,
			{ number = c.v }
		)
	end
	return rows
end

-- `v` as `format`: what is shown, and what is copied.
local function formatted(v, format)
	if format == "hex" then
		local s = hex(v)
		return s, s
	elseif format == "oct" then
		local s = oct(v)
		return s, s
	elseif format == "bin" then
		local plain, grouped = bin(v)
		return grouped, plain
	elseif format == "dec" then
		return pretty(v), raw(v)
	elseif format == "sci" then
		local s = string.format("%." .. math.max(opts.precision - 1, 0) .. "e", v)
		local m, e = s:match("^(.-)e(.*)$")
		if m:find("%.") then
			m = m:gsub("0+$", ""):gsub("%.$", "")
		end
		e = e:gsub("^%+?(%-?)0*(%d)", "%1%2")
		return m .. " × 10" .. e:gsub(".", SUPER), m .. "e" .. e
	elseif format == "frac" then
		local s = fraction(v, 1000000)
		return s, s
	elseif format == "roman" then
		local s = roman(v)
		return s, s
	end
end

local NAMES = {
	hex = "hexadecimal",
	bin = "binary",
	oct = "octal",
	dec = "decimal",
	sci = "scientific",
	frac = "fraction",
	roman = "roman numerals",
}

local function results(query)
	local r = evaluate(query)
	local v, rows = r.value, {}
	local expr = query:gsub("^%s+", ""):gsub("%s+$", "")
	if r.unit and r.to and r.to.unit then
		local to = r.to.unit
		if to.dim ~= r.unit.dim then
			fail("cannot convert " .. r.unit.dim .. " to " .. to.dim)
		end
		local c = convert(v, r.unit, to)
		rows[1] = row("result", pretty(c) .. " " .. to.sym, raw(c), pretty(v) .. " " .. r.unit.sym, { number = c })
		if opts.extras then
			for _, extra in ipairs(conversions(v, r.unit, 3)) do
				if extra.id ~= "unit:" .. to.sym then
					rows[#rows + 1] = extra
				end
			end
		end
	elseif r.unit then
		if r.to then
			fail("a quantity has no " .. r.to.format)
		end
		rows = conversions(v, r.unit, opts.extras and 4 or 1)
		if rows[1] then
			rows[1].id = "result"
		end
	elseif r.to and r.to.unit then
		fail("no unit to convert from")
	elseif r.to then
		local shown, copied = formatted(v, r.to.format)
		if not shown then
			fail("no " .. NAMES[r.to.format] .. " for " .. raw(v))
		end
		rows[1] = row("result", shown, copied, pretty(v) .. " in " .. NAMES[r.to.format], { number = v })
	else
		local title = pretty(v)
		if r.assign then
			title = r.assign .. " = " .. title
		end
		rows[1] = row("result", title, raw(v), expr, { number = v, assign = r.assign })
		if opts.extras and not r.assign then
			if integral(v) and math.abs(v) < 2 ^ 53 then
				-- Bases when the query is about them, or the number is big enough.
				local bases = expr:find("0[xXbBoO]")
					or expr:find("[&|~]")
					or expr:find("<<")
					or expr:find(">>")
					or expr:find("xor")
					or math.abs(v) >= 256
				if bases then
					local h = hex(v)
					local plain, grouped = bin(v)
					rows[#rows + 1] = row("hex", h, h, NAMES.hex, { number = v })
					rows[#rows + 1] = row("bin", grouped, plain, NAMES.bin, { number = v })
				end
			else
				local f = fraction(v, 10000)
				if f then
					rows[#rows + 1] = row("frac", f, f, NAMES.frac, { number = v })
				end
			end
		end
	end
	for _, t in ipairs(rows) do
		t.expr = expr
	end
	return rows
end

-- What an empty query shows: the results copied before, else a hint.
local function recent()
	if #history == 0 then
		return {
			row("hint", "Calculator", nil, "Try 2^10, sqrt 2, 15% of 80, 10 km in mi, 0xff in bin", { hint = true }),
		}
	end
	local rows = {}
	for i, h in ipairs(history) do
		rows[i] = row("history:" .. i, h.title, h.value, h.expr, { number = h.number, expr = h.expr })
	end
	return rows
end

local function remember(item)
	for i = #history, 1, -1 do
		if history[i].expr == item.expr and history[i].title == item.title then
			table.remove(history, i)
		end
	end
	table.insert(history, 1, { expr = item.expr, title = item.title, value = item.value, number = item.number })
	while #history > opts.history do
		table.remove(history)
	end
end

return {
	api = 1,
	setup = function(o)
		if type(o.precision) == "number" then
			opts.precision = math.max(1, math.min(17, math.floor(o.precision)))
		end
		if o.angle == "deg" or o.angle == "rad" then
			opts.angle = o.angle
		end
		if type(o.grouping) == "boolean" then
			opts.grouping = o.grouping
		end
		if type(o.extras) == "boolean" then
			opts.extras = o.extras
		end
		if type(o.history) == "number" then
			opts.history = math.max(0, math.floor(o.history))
		end
	end,
	sources = {
		{
			id = "calc",
			name = "Calculator",
			trigger = "=",
			icon = ICON,
			search = function(query, emit)
				if query:match("^%s*$") then
					return emit(recent())
				end
				local ok, rows = pcall(results, query)
				if ok then
					return emit(rows)
				end
				-- Half-typed (`2 * (3 +`): the last result stays until it
				-- makes sense again; a real mistake shows nothing.
				if type(rows) == "table" and rows.incomplete then
					return
				end
				emit({})
			end,
			run = function(item)
				if item.hint or not item.value then
					return
				end
				dock.clipboard(item.value)
				if item.number then
					ans = item.number
				end
				if item.assign then
					vars[item.assign] = item.number
				end
				remember(item)
			end,
		},
	},
}
