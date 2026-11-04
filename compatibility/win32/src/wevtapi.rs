//! wevtapi.dll: the Windows Event Log API over the logs of eventlog.rs.
//! The Application channel holds this process's reports; System, Security,
//! Setup and ForwardedEvents are empty, as OSyx has no event log service
//! (Chromium looks in System for the event log's own start and shutdown
//! events). Queries take the XPath filters programs use on these logs:
//! "*", and "*[System[...]]" over Provider[@Name='...'], EventID, Level,
//! Task and EventRecordID compared with = != < <= > >=, joined by and/or
//! and parentheses. Events render as XML or as values (system properties,
//! the event data strings, or Event/System/... paths). Remote sessions,
//! log files, subscriptions and bookmarks are not supported.
#![allow(non_snake_case)]
use crate::{error::*, eventlog::EventView, kernel32::fail, path};
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use os_std::sync::Mutex;

const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_EVT_INVALID_QUERY: u32 = 15001;
const ERROR_EVT_CHANNEL_NOT_FOUND: u32 = 15007;

const EVT_QUERY_CHANNEL_PATH: u32 = 0x1;
const EVT_QUERY_FILE_PATH: u32 = 0x2;
const EVT_QUERY_REVERSE_DIRECTION: u32 = 0x200;

const EVT_RENDER_CONTEXT_VALUES: u32 = 0;
const EVT_RENDER_CONTEXT_SYSTEM: u32 = 1;
const EVT_RENDER_CONTEXT_USER: u32 = 2;
const EVT_RENDER_EVENT_VALUES: u32 = 0;
const EVT_RENDER_EVENT_XML: u32 = 1;

// EVT_VARIANT_TYPE
const VAR_NULL: u32 = 0;
const VAR_STRING: u32 = 1;
const VAR_BYTE: u32 = 4;
const VAR_UINT16: u32 = 6;
const VAR_UINT64: u32 = 10;
const VAR_FILETIME: u32 = 17;
const VAR_HEXINT64: u32 = 21;

/// Keywords of a classic (ReportEvent) event: EventLogClassic.
const KEYWORDS_CLASSIC: u64 = 0x0080_0000_0000_0000;

/// Channels by their canonical names; only Application has events.
const CHANNELS: [&str; 5] = ["Application", "System", "Security", "Setup", "ForwardedEvents"];

enum Handle {
    Query { events: Vec<EventView>, next: usize },
    Event { event: EventView, channel: &'static str },
    Context { kind: u32, paths: Vec<String> },
}

static HANDLES: Mutex<BTreeMap<u64, Handle>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u64> = Mutex::new(0x7e10_0004);

fn insert(h: Handle) -> u64 {
    let mut next = NEXT.lock().unwrap();
    let id = *next;
    *next += 4;
    drop(next);
    HANDLES.lock().unwrap().insert(id, h);
    id
}

// ---- the XPath filter --------------------------------------------------------------------------

/// One comparison or a combination of them.
enum Filter {
    All,
    Provider(String),
    Field(&'static str, Cmp, u64),
    And(alloc::boxed::Box<Filter>, alloc::boxed::Box<Filter>),
    Or(alloc::boxed::Box<Filter>, alloc::boxed::Box<Filter>),
}

#[derive(Clone, Copy)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn skip(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn eat(&mut self, t: &str) -> bool {
        self.skip();
        if self.s[self.at..].starts_with(t.as_bytes()) {
            self.at += t.len();
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> &str {
        self.skip();
        let start = self.at;
        while self.at < self.s.len() && (self.s[self.at].is_ascii_alphanumeric() || self.s[self.at] == b'_') {
            self.at += 1;
        }
        core::str::from_utf8(&self.s[start..self.at]).unwrap_or("")
    }

    /// The operator word `k` (not the start of a longer name).
    fn keyword(&mut self, k: &str) -> bool {
        self.skip();
        let end = self.at + k.len();
        if self.s[self.at..].starts_with(k.as_bytes()) && self.s.get(end).is_none_or(|c| !c.is_ascii_alphanumeric()) {
            self.at = end;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Option<Filter> {
        let mut f = self.and()?;
        while self.keyword("or") {
            f = Filter::Or(alloc::boxed::Box::new(f), alloc::boxed::Box::new(self.and()?));
        }
        Some(f)
    }

    fn and(&mut self) -> Option<Filter> {
        let mut f = self.factor()?;
        while self.keyword("and") {
            f = Filter::And(alloc::boxed::Box::new(f), alloc::boxed::Box::new(self.factor()?));
        }
        Some(f)
    }

    fn factor(&mut self) -> Option<Filter> {
        if self.eat("(") {
            let f = self.or()?;
            return self.eat(")").then_some(f);
        }
        let name = String::from(self.word());
        if name == "Provider" {
            if !(self.eat("[") && self.eat("@Name") && self.eat("=")) {
                return None;
            }
            self.skip();
            let quote = *self.s.get(self.at)?;
            if quote != b'\'' && quote != b'"' {
                return None;
            }
            let end = self.at + 1 + self.s[self.at + 1..].iter().position(|&c| c == quote)?;
            let value = String::from(core::str::from_utf8(&self.s[self.at + 1..end]).ok()?);
            self.at = end + 1;
            return self.eat("]").then_some(Filter::Provider(value));
        }
        let field = match name.as_str() {
            "EventID" => "EventID",
            "Level" => "Level",
            "Task" => "Task",
            "EventRecordID" => "EventRecordID",
            _ => return None,
        };
        let cmp = if self.eat("!=") {
            Cmp::Ne
        } else if self.eat("<=") || self.eat("&lt;=") {
            Cmp::Le
        } else if self.eat(">=") || self.eat("&gt;=") {
            Cmp::Ge
        } else if self.eat("<") || self.eat("&lt;") {
            Cmp::Lt
        } else if self.eat(">") || self.eat("&gt;") {
            Cmp::Gt
        } else if self.eat("=") {
            Cmp::Eq
        } else {
            return None;
        };
        let value = self.word().parse().ok()?;
        Some(Filter::Field(field, cmp, value))
    }
}

/// The filter of an XPath query, None when it is not one this supports.
fn parse(query: &str) -> Option<Filter> {
    let q = query.trim();
    if q == "*" || q.is_empty() {
        return Some(Filter::All);
    }
    let mut p = Parser { s: q.as_bytes(), at: 0 };
    if !(p.eat("*") && p.eat("[") && p.eat("System") && p.eat("[")) {
        return None;
    }
    let f = p.or()?;
    (p.eat("]") && p.eat("]") && { p.skip(); p.at == p.s.len() }).then_some(f)
}

/// The level of a classic event type: error 2, warning 3, information 4,
/// audits 0 (LogAlways).
fn level(kind: u16) -> u64 {
    match kind {
        1 => 2,
        2 => 3,
        8 | 16 => 0,
        _ => 4,
    }
}

fn matches(f: &Filter, e: &EventView) -> bool {
    match f {
        Filter::All => true,
        Filter::Provider(name) => e.source.eq_ignore_ascii_case(name),
        Filter::Field(field, cmp, v) => {
            let x = match *field {
                "EventID" => (e.id & 0xFFFF) as u64,
                "Level" => level(e.kind),
                "Task" => e.category as u64,
                _ => e.number as u64,
            };
            match cmp {
                Cmp::Eq => x == *v,
                Cmp::Ne => x != *v,
                Cmp::Lt => x < *v,
                Cmp::Le => x <= *v,
                Cmp::Gt => x > *v,
                Cmp::Ge => x >= *v,
            }
        }
        Filter::And(a, b) => matches(a, e) && matches(b, e),
        Filter::Or(a, b) => matches(a, e) || matches(b, e),
    }
}

// ---- queries -------------------------------------------------------------------------------------

/// The channel a query handle reads, kept with each event it hands out.
static QUERY_CHANNEL: Mutex<BTreeMap<u64, &'static str>> = Mutex::new(BTreeMap::new());

extern "win64" fn EvtQuery(session: u64, channel: *const u16, query: *const u16, flags: u32) -> u64 {
    if session != 0 {
        // No session can exist: EvtOpenSession (remote logs) is not offered.
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    // SAFETY: NULL or NUL-terminated strings.
    let (channel, query) = unsafe { (path::wide_to_string(channel), path::wide_to_string(query)) };
    let Some(channel) = channel else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if flags & EVT_QUERY_FILE_PATH != 0 {
        let exists = path::to_osyx(&channel, &crate::kernel32::cwd_win()).is_ok_and(|p| os_sys::stat(&p).is_ok());
        return fail(if exists { ERROR_NOT_SUPPORTED } else { ERROR_FILE_NOT_FOUND }, 0);
    }
    if flags & EVT_QUERY_CHANNEL_PATH == 0 && flags & 0x3 != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(name) = CHANNELS.iter().find(|c| c.eq_ignore_ascii_case(&channel)).copied() else {
        return fail(ERROR_EVT_CHANNEL_NOT_FOUND, 0);
    };
    let Some(filter) = parse(query.as_deref().unwrap_or("*")) else { return fail(ERROR_EVT_INVALID_QUERY, 0) };
    let mut events: Vec<EventView> = if name == "Application" {
        crate::eventlog::application_events().into_iter().filter(|e| matches(&filter, e)).collect()
    } else {
        Vec::new()
    };
    if flags & EVT_QUERY_REVERSE_DIRECTION != 0 {
        events.reverse();
    }
    let h = insert(Handle::Query { events, next: 0 });
    QUERY_CHANNEL.lock().unwrap().insert(h, name);
    h
}

/// The next events of a query, at most `count`; ERROR_NO_MORE_ITEMS when
/// none are left.
extern "win64" fn EvtNext(query: u64, count: u32, out: *mut u64, _timeout: u32, flags: u32, returned: *mut u32) -> i32 {
    if out.is_null() || returned.is_null() || flags != 0 || count == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's count.
    unsafe { *returned = 0 };
    let channel = QUERY_CHANNEL.lock().unwrap().get(&query).copied().unwrap_or("Application");
    let taken: Vec<EventView> = {
        let mut handles = HANDLES.lock().unwrap();
        let Some(Handle::Query { events, next }) = handles.get_mut(&query) else { return fail(ERROR_INVALID_HANDLE, 0) };
        let end = (*next + count as usize).min(events.len());
        let taken = events[*next..end].to_vec();
        *next = end;
        taken
    };
    if taken.is_empty() {
        return fail(ERROR_NO_MORE_ITEMS, 0);
    }
    for (i, event) in taken.into_iter().enumerate() {
        let h = insert(Handle::Event { event, channel });
        // SAFETY: the caller's array of `count` handles.
        unsafe {
            *out.add(i) = h;
            *returned = i as u32 + 1;
        }
    }
    1
}

extern "win64" fn EvtCreateRenderContext(count: u32, paths: *const u64, flags: u32) -> u64 {
    let kind = flags;
    if kind > EVT_RENDER_CONTEXT_USER || (kind == EVT_RENDER_CONTEXT_VALUES && count > 0 && paths.is_null()) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut list = Vec::new();
    if kind == EVT_RENDER_CONTEXT_VALUES {
        for i in 0..count as usize {
            // SAFETY: the caller's array of `count` string pointers.
            let p = unsafe { *paths.add(i) } as *const u16;
            // SAFETY: a NUL-terminated path.
            list.push(unsafe { path::wide_to_string(p) }.unwrap_or_default());
        }
    }
    insert(Handle::Context { kind, paths: list })
}

// ---- rendering -----------------------------------------------------------------------------------

/// 100 ns units since 1601 for seconds since 1970.
fn filetime(secs: u32) -> u64 {
    secs as u64 * 10_000_000 + 116_444_736_000_000_000
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The event as Windows renders a classic event's XML.
fn xml(e: &EventView, channel: &str) -> String {
    let days = (e.time / 86_400) as i64;
    let (y, m, d) = crate::sysext::civil(days);
    let t = e.time % 86_400;
    let data: String = e.strings.iter().map(|s| format!("<Data>{}</Data>", escape(s))).collect();
    format!(
        "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='{}'/><EventID Qualifiers='{}'>{}</EventID>\
         <Version>0</Version><Level>{}</Level><Task>{}</Task><Opcode>0</Opcode><Keywords>{KEYWORDS_CLASSIC:#x}</Keywords>\
         <TimeCreated SystemTime='{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.0000000Z'/><EventRecordID>{}</EventRecordID><Correlation/>\
         <Execution ProcessID='0' ThreadID='0'/><Channel>{channel}</Channel><Computer>{}</Computer><Security/></System>\
         <EventData>{data}</EventData></Event>",
        escape(&e.source),
        e.id >> 16,
        e.id & 0xFFFF,
        level(e.kind),
        e.category,
        t / 3600,
        t / 60 % 60,
        t % 60,
        e.number,
        escape(&crate::accounts::computer_name()),
    )
}

/// An EVT_VARIANT value before layout: strings are placed after the array.
enum Value {
    Null,
    Str(String),
    Num(u32, u64),
}

/// The value of an "Event/System/..." path (or Event/EventData/Data[n]).
fn path_value(e: &EventView, channel: &str, p: &str) -> Value {
    let p = p.trim().trim_start_matches('/');
    let p = p.strip_prefix("Event/").unwrap_or(p);
    match p {
        "System/Provider/@Name" => Value::Str(e.source.clone()),
        "System/EventID" => Value::Num(VAR_UINT16, (e.id & 0xFFFF) as u64),
        "System/EventID/@Qualifiers" => Value::Num(VAR_UINT16, (e.id >> 16) as u64),
        "System/Level" => Value::Num(VAR_BYTE, level(e.kind)),
        "System/Task" => Value::Num(VAR_UINT16, e.category as u64),
        "System/Keywords" => Value::Num(VAR_HEXINT64, KEYWORDS_CLASSIC),
        "System/TimeCreated/@SystemTime" => Value::Num(VAR_FILETIME, filetime(e.time)),
        "System/EventRecordID" => Value::Num(VAR_UINT64, e.number as u64),
        "System/Channel" => Value::Str(String::from(channel)),
        "System/Computer" => Value::Str(crate::accounts::computer_name()),
        _ => match p.strip_prefix("EventData/Data[").and_then(|r| r.strip_suffix(']')).and_then(|n| n.parse::<usize>().ok()) {
            // XPath positions start at 1.
            Some(n) if n >= 1 && n <= e.strings.len() => Value::Str(e.strings[n - 1].clone()),
            _ => Value::Null,
        },
    }
}

/// The 18 EVT_SYSTEM_PROPERTY_ID values; a classic event has no provider
/// GUID, opcode, activity, process, thread, user or version.
fn system_values(e: &EventView, channel: &str) -> Vec<Value> {
    alloc::vec![
        Value::Str(e.source.clone()),
        Value::Null,
        Value::Num(VAR_UINT16, (e.id & 0xFFFF) as u64),
        Value::Num(VAR_UINT16, (e.id >> 16) as u64),
        Value::Num(VAR_BYTE, level(e.kind)),
        Value::Num(VAR_UINT16, e.category as u64),
        Value::Null,
        Value::Num(VAR_HEXINT64, KEYWORDS_CLASSIC),
        Value::Num(VAR_FILETIME, filetime(e.time)),
        Value::Num(VAR_UINT64, e.number as u64),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Str(String::from(channel)),
        Value::Str(crate::accounts::computer_name()),
        Value::Null,
        Value::Null,
    ]
}

/// Lays out EVT_VARIANTs (16 bytes: value, count, type) followed by their
/// strings, as they will sit at `base`.
fn layout(values: &[Value], base: u64) -> Vec<u8> {
    let mut out = alloc::vec![0u8; values.len() * 16];
    for (i, v) in values.iter().enumerate() {
        let (value, kind) = match v {
            Value::Null => (0, VAR_NULL),
            Value::Num(kind, n) => (*n, *kind),
            Value::Str(s) => {
                let at = base + out.len() as u64;
                out.extend(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
                (at, VAR_STRING)
            }
        };
        out[i * 16..i * 16 + 8].copy_from_slice(&value.to_le_bytes());
        out[i * 16 + 12..i * 16 + 16].copy_from_slice(&kind.to_le_bytes());
    }
    out
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn EvtRender(context: u64, fragment: u64, flags: u32, size: u32, buf: *mut u8, used: *mut u32, count: *mut u32) -> i32 {
    if used.is_null() || count.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let handles = HANDLES.lock().unwrap();
    let Some(Handle::Event { event, channel }) = handles.get(&fragment) else { return fail(ERROR_INVALID_HANDLE, 0) };
    let (bytes, n) = match flags {
        EVT_RENDER_EVENT_XML => (xml(event, channel).encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect::<Vec<u8>>(), 0),
        EVT_RENDER_EVENT_VALUES => {
            let values = match handles.get(&context) {
                Some(Handle::Context { kind: EVT_RENDER_CONTEXT_SYSTEM, .. }) => system_values(event, channel),
                Some(Handle::Context { kind: EVT_RENDER_CONTEXT_USER, .. }) => event.strings.iter().map(|s| Value::Str(s.clone())).collect(),
                Some(Handle::Context { paths, .. }) => paths.iter().map(|p| path_value(event, channel, p)).collect(),
                _ => return fail(ERROR_INVALID_HANDLE, 0),
            };
            let n = values.len() as u32;
            (layout(&values, buf as u64), n)
        }
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    drop(handles);
    // SAFETY: the caller's out values.
    unsafe {
        *used = bytes.len() as u32;
        *count = n;
    }
    if buf.is_null() || (size as usize) < bytes.len() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len()) };
    1
}

extern "win64" fn EvtClose(h: u64) -> i32 {
    QUERY_CHANNEL.lock().unwrap().remove(&h);
    match HANDLES.lock().unwrap().remove(&h) {
        Some(_) => 1,
        None => fail(ERROR_INVALID_HANDLE, 0),
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![EvtQuery, EvtNext, EvtCreateRenderContext, EvtRender, EvtClose]
}
