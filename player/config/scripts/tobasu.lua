-- Tobasu: OP/ED aralıklarını chapter olarak işaretler, ayar açıkken otomatik atlar
-- ve atlayınca köşede küçük bir bildirim gösterir. "sonu" açıksa ED'den sonrasını
-- (bölüm sonu özeti/fragman) hiç oynatmadan sonraki bölüme geçer. Seçilen hızı oturumlar arasında hatırlar.
-- Hangi bölümde hangi dakikada kalındığını uygulamanın okuduğu izleme dosyasına yazar.
--
-- Tuşlar:  s      otomatik atlamayı aç/kapat
--          S      bölüm sonu (ED sonrası) atlamayı aç/kapat
--          ENTER  OP/ED içindeyken hemen atla (dışındayken sonraki bölüm)

local mp = require "mp"
local utils = require "mp.utils"
local options = require "mp.options"

local o = { veri = "", oto = true, sonu = false, izleme = "", dil = "tr" }
options.read_options(o, "tobasu")

-- Bildirim metni: dil "en" ise İngilizcesi
local function L(tr, en) return o.dil == "en" and en or tr end

local data = {}
local cur = nil        -- çalan bölümün { op = {s, e}, ed = {s, e}, title = "" } kaydı
local last_t = nil     -- bir önceki zaman; aralığa "girildiği" anı yakalamak için
local overlay = mp.create_osd_overlay("ass-events")
local hide_timer = nil
local speed_file = mp.command_native({ "expand-path", "~~/hiz.txt" })
local watch = { eps = {} }  -- izleme dosyası: { last = eid, eps = { [eid] = { pos, dur, done, t } } }
local last_save = 0

local function load_data()
    if o.veri == "" then return end
    local f = io.open(o.veri, "r")
    if not f then return end
    data = utils.parse_json(f:read("*a")) or {}
    f:close()
end

local function fmt(t)
    t = math.floor(t + 0.5)
    return string.format("%d:%02d", math.floor(t / 60), t % 60)
end

local function notify(text)
    local w, h = mp.get_osd_size()
    if not w or w == 0 then w, h = 1280, 720 end
    overlay.res_x, overlay.res_y = w, h
    local fs = math.max(16, math.floor(h / 40))
    overlay.data = string.format(
        "{\\an9\\pos(%d,%d)\\fs%d\\bord%.1f\\shad0\\1c&HFFFFFF&\\3c&H202020&\\alpha&H18&}%s",
        w - math.floor(fs * 0.9), math.floor(fs * 0.8), fs, fs / 12, text)
    overlay:update()
    if hide_timer then hide_timer:kill() end
    hide_timer = mp.add_timeout(2.2, function() overlay:remove() end)
end

local function lookup(path)
    if not path then return nil end
    return data[path] or data[(path:gsub("/", "\\"))]
end

local function set_chapters()
    if not cur then return end
    local list = {}
    local existing = mp.get_property_native("chapter-list") or {}
    if #existing == 0 then
        table.insert(list, { time = 0, title = "Bölüm" })
    else
        for _, c in ipairs(existing) do table.insert(list, c) end
    end
    local function add(r, name)
        if not r then return end
        table.insert(list, { time = r[1], title = name })
        table.insert(list, { time = r[2], title = "Bölüm" })
    end
    add(cur.op, "Opening")
    add(cur.ed, "Ending")
    table.sort(list, function(a, b) return a.time < b.time end)
    mp.set_property_native("chapter-list", list)
end

-- Bölümün kalanını atlar: sonraki bölüme geçer, son bölümse dosyanın sonuna sarar
local function next_ep(text)
    local pos = mp.get_property_number("playlist-pos", 0)
    local count = mp.get_property_number("playlist-count", 1)
    if pos + 1 < count then
        mp.command("playlist-next")
        notify(text .. L(" · sonraki bölüm", " · next episode"))
    else
        mp.commandv("seek", "100", "absolute-percent+exact")
        notify(text)
    end
end

local function skip_to(kind, r, auto)
    mp.msg.info(string.format("%s atlandı: %.2f -> %.2f", kind, r[1], r[2]))
    local dur = mp.get_property_number("duration") or 0
    local pos = mp.get_property_number("playlist-pos", 0)
    local count = mp.get_property_number("playlist-count", 1)
    local label = kind == "op" and "OP" or "ED"
    if kind == "ed" and o.sonu then
        next_ep(L("ED ve bölüm sonu atlandı", "Skipped ED and preview"))
        return
    end
    if kind == "ed" and dur > 0 and r[2] >= dur - 1.5 and pos + 1 < count then
        mp.command("playlist-next")
        notify(label .. L(" atlandı · sonraki bölüm", " skipped · next episode"))
        return
    end
    mp.commandv("seek", tostring(r[2]), "absolute+exact")
    notify(label .. L(" atlandı · ", " skipped · ") .. fmt(r[2] - r[1]) .. (auto and "" or L(" (elle)", " (manual)")))
end

-- ---------------------------------------------------------------- izleme kaydı

local function load_watch()
    if o.izleme == "" then return end
    local f = io.open(o.izleme, "r")
    if not f then return end
    local d = utils.parse_json(f:read("*a"))
    f:close()
    if type(d) == "table" and type(d.eps) == "table" then watch = d end
end

local function write_watch()
    if o.izleme == "" then return end
    local tmp = o.izleme .. ".tmp"
    local f = io.open(tmp, "w")
    if not f then return end
    f:write(utils.format_json(watch) or "{}")
    f:close()
    os.remove(o.izleme)  -- Windows'ta rename hedefin üzerine yazmaz
    os.rename(tmp, o.izleme)
end

-- Bölüm ED'ye ya da son %8'e gelince izlendi sayılır
local function record(force, eof)
    if not cur or not cur.id or not last_t then return end
    local dur = mp.get_property_number("duration") or (watch.eps[cur.id] or {}).dur or 0
    local w = watch.eps[cur.id] or {}
    local finished = eof or (dur > 0 and last_t >= dur * 0.92) or (cur.ed and last_t >= cur.ed[1])
    w.pos = finished and dur or last_t
    w.dur = dur
    w.done = w.done or finished or false
    w.t = os.time()
    watch.eps[cur.id] = w
    watch.last = cur.id
    local now = mp.get_time()
    if force or now - last_save >= 5 then
        last_save = now
        write_watch()
    end
end

mp.register_event("file-loaded", function()
    cur = lookup(mp.get_property("path"))
    last_t = nil
    if not cur then return end
    if cur.title and cur.title ~= "" then
        mp.set_property("force-media-title", cur.title)
    end
    set_chapters()
    -- Yarıda kalmış bölüm kaldığı yerden başlar (mpv kendi konumunu zaten yüklediyse dokunma)
    local w = cur.id and watch.eps[cur.id]
    local t = mp.get_property_number("time-pos") or 0
    if w and not w.done and w.pos and w.pos > 10 and t < 5 then
        mp.commandv("seek", tostring(w.pos - 3), "absolute")
        notify(L("Kaldığın yerden: ", "Resuming at ") .. fmt(w.pos))
    end
end)

mp.register_event("end-file", function(ev)
    record(true, ev.reason == "eof")
    cur = nil
end)

mp.register_event("shutdown", function() record(true, false) end)

mp.observe_property("pause", "bool", function(_, p)
    if p then record(true, false) end
end)

mp.observe_property("time-pos", "number", function(_, t)
    if not t then return end
    local prev = last_t
    last_t = t
    record(false, false)
    if not cur then return end
    -- ED bitip özet/fragman başlayınca (dışarıdan oraya sarılmadıysa) bölümü bitir
    local ed = cur.ed
    if o.sonu and ed and prev ~= nil and prev < ed[2] - 0.5 and t >= ed[2] - 0.5 then
        local dur = mp.get_property_number("duration") or 0
        if dur <= 0 or ed[2] < dur - 1.5 then
            next_ep(L("Bölüm sonu atlandı", "Skipped preview"))
            return
        end
    end
    if not o.oto then return end
    for _, kind in ipairs({ "op", "ed" }) do
        local r = cur[kind]
        -- Yalnızca aralığa dışarıdan girilince atla; kullanıcı içine sararsa dokunma
        if r and t >= r[1] and t < r[2] - 0.5 then
            local entered = (prev == nil and t <= r[1] + 1.5) or (prev ~= nil and prev < r[1])
            if entered then
                skip_to(kind, r, true)
                return
            end
        end
    end
end)

mp.add_forced_key_binding("s", "tobasu-oto", function()
    o.oto = not o.oto
    notify(L("Otomatik atlama: ", "Auto skip: ") .. (o.oto and L("açık", "on") or L("kapalı", "off")))
end)

mp.add_forced_key_binding("S", "tobasu-sonu", function()
    o.sonu = not o.sonu
    notify(L("Bölüm sonu atlama: ", "Preview skip: ") .. (o.sonu and L("açık", "on") or L("kapalı", "off")))
end)

mp.add_forced_key_binding("ENTER", "tobasu-simdi", function()
    local t = mp.get_property_number("time-pos")
    if cur and t then
        for _, kind in ipairs({ "op", "ed" }) do
            local r = cur[kind]
            if r and t >= r[1] - 1 and t < r[2] then
                skip_to(kind, r, false)
                return
            end
        end
    end
    mp.command("playlist-next")
end)

-- Hız: son seçilen hız kaydedilir ve bir sonraki açılışta kullanılır
do
    local f = io.open(speed_file, "r")
    if f then
        local v = tonumber(f:read("*l") or "")
        f:close()
        if v and v >= 0.25 and v <= 8 then mp.set_property_number("speed", v) end
    end
end
mp.observe_property("speed", "number", function(_, v)
    if not v then return end
    local f = io.open(speed_file, "w")
    if f then f:write(string.format("%.2f\n", v)) f:close() end
end)

load_data()
load_watch()
