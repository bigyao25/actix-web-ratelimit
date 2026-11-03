local time = redis.call("TIME")
local now = tonumber(time[1]) * 1000000 + tonumber(time[2])
local window = tonumber(ARGV[1])
local limit = tonumber(ARGV[2])

-- Keep timestamps monotonic even if the Redis server clock moves backwards.
local latest = redis.call("ZREVRANGE", KEYS[1], 0, 0, "WITHSCORES")
if #latest > 0 then
    now = math.max(now, tonumber(latest[2]))
end

redis.call("ZREMRANGEBYSCORE", KEYS[1], "-inf", string.format("%.0f", now - window))
local count = redis.call("ZCARD", KEYS[1])
if count >= limit then
    return 1
end

-- At a fixed monotonic timestamp, expired members cannot change and count only
-- increases. The timestamp/count pair is therefore unique, across all instances.
local member = string.format("%.0f", now) .. ":" .. count
redis.call("ZADD", KEYS[1], now, member)
redis.call("PEXPIRE", KEYS[1], math.ceil(window / 1000))
return 0
