//! База знаний об экспортах системных библиотек PS4.
//!
//! В настоящих играх импорт идёт по NID (идентификаторам, вычисляемым из
//! имени функции), поэтому здесь предусмотрены:
//!  * таблица «известных» имён функций с указанием модуля;
//!  * загрузка пользовательской таблицы `nids.json` (NID -> имя);
//!  * функция вычисления NID из имени (SHA-1 + перестановка байт — так Sony
//!    формирует идентификаторы экспорта).

use std::collections::BTreeMap;

/// Модули системных библиотек, которые встречаются в играх.
pub const MODULES: &[&str] = &[
    "libkernel",
    "libSceLibcInternal",
    "libSceSysmodule",
    "libSceUserService",
    "libSceSystemService",
    "libScePad",
    "libSceVideoOut",
    "libSceAudioOut",
    "libSceAudioIn",
    "libSceRtc",
    "libSceSaveData",
    "libSceNet",
    "libSceNetCtl",
    "libSceHttp",
    "libSceSsl",
    "libSceNpManager",
    "libSceNpScore",
    "libSceAppContent",
    "libSceCommonDialog",
    "libSceMsgDialog",
    "libSceImeDialog",
    "libSceSysUtil",
    "libSceGnmDriver",
    "libSceGpu",
    "libSceAjm",
    "libSceVdec",
    "libSceVenc",
    "libSceFont",
    "libSceFontFt",
    "libSceFreeType",
    "libSceNgs2",
    "libSceJson2",
    "libSceKeyboard",
    "libSceMouse",
    "libSceCamera",
    "libSceMove",
    "libSceShare",
    "libSceUsbd",
    "libSceZlib",
    "libScePng",
    "libSceJpeg",
    "libSceGameUpdate",
    "libScePlayGo",
    "libSceNotification",
    "libSceErrorDialog",
    "libSceSaveDataDialog",
    "libSceWebBrowserDialog",
    "libSceNpAuth",
    "libSceNpCommon",
    "libSceNpMatching2",
    "libSceNpWebApi",
    "libSceScreenShot",
    "libSceVideoRecording",
    "libSceVoice",
    "libSceUlobjMgr",
    "libSceSysCore",
    "libSceAudiodec",
    "libSceAudiodecCpu",
    "libSceVideodec",
    "libSceVideodec2",
    "libSceIme",
    "libSceNetCtl",
];

/// Известные имена экспортов (без NID). Список отражает часто используемый
/// играми API; его наличие позволяет отличать «неизвестный символ» от
/// «известный, но ещё не реализованный».
pub const KNOWN_EXPORTS: &[(&str, &str)] = &[
    // libkernel
    ("sceKernelGetProcessId", "libkernel"),
    ("sceKernelGetThreadId", "libkernel"),
    ("sceKernelGetCurrentCpu", "libkernel"),
    ("sceKernelGetProcessTime", "libkernel"),
    ("sceKernelGetProcessTimeLow", "libkernel"),
    ("sceKernelGetProcessTimeCounter", "libkernel"),
    ("sceKernelGetProcessTimeCounterFrequency", "libkernel"),
    ("sceKernelUsleep", "libkernel"),
    ("sceKernelSleep", "libkernel"),
    ("sceKernelClockGettime", "libkernel"),
    ("sceKernelGettimeofday", "libkernel"),
    ("sceKernelGetSystemTime", "libkernel"),
    ("sceKernelGetSystemTimeLow", "libkernel"),
    ("sceKernelCreateEqueue", "libkernel"),
    ("sceKernelDeleteEqueue", "libkernel"),
    ("sceKernelWaitEqueue", "libkernel"),
    ("sceKernelAddUserEvent", "libkernel"),
    ("sceKernelAddUserEventEdge", "libkernel"),
    ("sceKernelTriggerUserEvent", "libkernel"),
    ("sceKernelDeleteUserEvent", "libkernel"),
    ("sceKernelCreateSema", "libkernel"),
    ("sceKernelDeleteSema", "libkernel"),
    ("sceKernelWaitSema", "libkernel"),
    ("sceKernelSignalSema", "libkernel"),
    ("sceKernelPollSema", "libkernel"),
    ("sceKernelCreateMutex", "libkernel"),
    ("sceKernelDeleteMutex", "libkernel"),
    ("sceKernelLockMutex", "libkernel"),
    ("sceKernelUnlockMutex", "libkernel"),
    ("sceKernelTryLockMutex", "libkernel"),
    ("sceKernelAllocateMainDirectMemory", "libkernel"),
    ("sceKernelAllocateDirectMemory", "libkernel"),
    ("sceKernelReleaseDirectMemory", "libkernel"),
    ("sceKernelMapDirectMemory", "libkernel"),
    ("sceKernelMapFlexibleMemory", "libkernel"),
    ("sceKernelMapNamedFlexibleMemory", "libkernel"),
    ("sceKernelMapNamedDirectMemory", "libkernel"),
    ("sceKernelAllocateFlexibleMemory", "libkernel"),
    ("sceKernelReleaseFlexibleMemory", "libkernel"),
    ("sceKernelGetDirectMemorySize", "libkernel"),
    ("sceKernelMapNamedSystemFlexibleMemory", "libkernel"),
    ("sceKernelMprotect", "libkernel"),
    ("sceKernelMtypeprotect", "libkernel"),
    ("sceKernelMunmap", "libkernel"),
    ("sceKernelLoadStartModule", "libkernel"),
    ("sceKernelLoadModule", "libkernel"),
    ("sceKernelStartModule", "libkernel"),
    ("sceKernelStopModule", "libkernel"),
    ("sceKernelUnloadModule", "libkernel"),
    ("sceKernelDlsym", "libkernel"),
    ("sceKernelGetThreadName", "libkernel"),
    ("sceKernelGetThreadInfo", "libkernel"),
    ("sceKernelGetThreadCpuAffinityMask", "libkernel"),
    ("sceKernelSetThreadCpuAffinityMask", "libkernel"),
    ("sceKernelYieldThread", "libkernel"),
    ("sceKernelGetAppInfo", "libkernel"),
    ("sceKernelGetModuleInfo", "libkernel"),
    ("sceKernelGetModuleList", "libkernel"),
    ("scePthreadCreate", "libkernel"),
    ("scePthreadJoin", "libkernel"),
    ("scePthreadDetach", "libkernel"),
    ("scePthreadExit", "libkernel"),
    ("scePthreadSelf", "libkernel"),
    ("scePthreadSetprio", "libkernel"),
    ("scePthreadRename", "libkernel"),
    ("scePthreadAttrInit", "libkernel"),
    ("scePthreadAttrDestroy", "libkernel"),
    ("scePthreadAttrSetstacksize", "libkernel"),
    ("scePthreadMutexInit", "libkernel"),
    ("scePthreadMutexDestroy", "libkernel"),
    ("scePthreadMutexLock", "libkernel"),
    ("scePthreadMutexUnlock", "libkernel"),
    ("scePthreadMutexTrylock", "libkernel"),
    ("scePthreadCondInit", "libkernel"),
    ("scePthreadCondDestroy", "libkernel"),
    ("scePthreadCondWait", "libkernel"),
    ("scePthreadCondSignal", "libkernel"),
    ("scePthreadCondBroadcast", "libkernel"),
    ("scePthreadCondTimedwait", "libkernel"),
    // libSceLibcInternal
    ("malloc", "libSceLibcInternal"),
    ("free", "libSceLibcInternal"),
    ("calloc", "libSceLibcInternal"),
    ("realloc", "libSceLibcInternal"),
    ("memalign", "libSceLibcInternal"),
    ("posix_memalign", "libSceLibcInternal"),
    ("memcpy", "libSceLibcInternal"),
    ("memmove", "libSceLibcInternal"),
    ("memset", "libSceLibcInternal"),
    ("memcmp", "libSceLibcInternal"),
    ("strlen", "libSceLibcInternal"),
    ("strcmp", "libSceLibcInternal"),
    ("strncmp", "libSceLibcInternal"),
    ("strcpy", "libSceLibcInternal"),
    ("strncpy", "libSceLibcInternal"),
    ("strcat", "libSceLibcInternal"),
    ("strchr", "libSceLibcInternal"),
    ("strrchr", "libSceLibcInternal"),
    ("strstr", "libSceLibcInternal"),
    ("strdup", "libSceLibcInternal"),
    ("strtol", "libSceLibcInternal"),
    ("strtod", "libSceLibcInternal"),
    ("atoi", "libSceLibcInternal"),
    ("printf", "libSceLibcInternal"),
    ("sprintf", "libSceLibcInternal"),
    ("snprintf", "libSceLibcInternal"),
    ("fprintf", "libSceLibcInternal"),
    ("puts", "libSceLibcInternal"),
    ("fwrite", "libSceLibcInternal"),
    ("fopen", "libSceLibcInternal"),
    ("fclose", "libSceLibcInternal"),
    ("exit", "libSceLibcInternal"),
    ("abort", "libSceLibcInternal"),
    ("__cxa_atexit", "libSceLibcInternal"),
    ("__stack_chk_fail", "libSceLibcInternal"),
    // прочие библиотеки
    ("sceSysmoduleLoadModule", "libSceSysmodule"),
    ("sceSysmoduleUnloadModule", "libSceSysmodule"),
    ("sceSysmoduleIsLoaded", "libSceSysmodule"),
    ("sceSysmoduleLoadModuleByNameInternal", "libSceSysmodule"),
    ("sceUserServiceInitialize", "libSceUserService"),
    ("sceUserServiceTerminate", "libSceUserService"),
    ("sceUserServiceGetInitialUser", "libSceUserService"),
    ("sceUserServiceGetUserName", "libSceUserService"),
    ("sceUserServiceGetLoginUserIdList", "libSceUserService"),
    ("scePadInit", "libScePad"),
    ("scePadOpen", "libScePad"),
    ("scePadClose", "libScePad"),
    ("scePadReadState", "libScePad"),
    ("scePadRead", "libScePad"),
    ("scePadGetHandle", "libScePad"),
    ("sceVideoOutOpen", "libSceVideoOut"),
    ("sceVideoOutClose", "libSceVideoOut"),
    ("sceVideoOutSetBufferAttribute", "libSceVideoOut"),
    ("sceVideoOutRegisterBuffers", "libSceVideoOut"),
    ("sceVideoOutSubmitFlip", "libSceVideoOut"),
    ("sceVideoOutWaitVblank", "libSceVideoOut"),
    ("sceVideoOutGetVblankStatus", "libSceVideoOut"),
    ("sceAudioOutInit", "libSceAudioOut"),
    ("sceAudioOutOpen", "libSceAudioOut"),
    ("sceAudioOutOutput", "libSceAudioOut"),
    ("sceAudioOutClose", "libSceAudioOut"),
    ("sceRtcGetCurrentTick", "libSceRtc"),
    ("sceRtcGetTickResolution", "libSceRtc"),
    ("sceRtcGetTick", "libSceRtc"),
    ("sceSaveDataInitialize", "libSceSaveData"),
    ("sceSaveDataSetupSaveDataMemory", "libSceSaveData"),
    ("sceSaveDataGetSaveDataMemory", "libSceSaveData"),
    ("sceSaveDataCommitSaveData", "libSceSaveData"),
    ("sceNetInit", "libSceNet"),
    ("sceNetPoolCreate", "libSceNet"),
    ("sceNetSocket", "libSceNet"),
    ("sceNetConnect", "libSceNet"),
    ("sceNetSend", "libSceNet"),
    ("sceNetRecv", "libSceNet"),
    ("sceNetCtlInit", "libSceNetCtl"),
    ("sceHttpInit", "libSceHttp"),
    ("sceHttpCreateTemplate", "libSceHttp"),
    ("sceHttpGetStatusCode", "libSceHttp"),
    ("sceSystemServiceGetStatus", "libSceSystemService"),
    ("sceSystemServiceParamGetInt", "libSceSystemService"),
    ("sceSystemServiceGetAppId", "libSceSystemService"),
    ("sceMsgDialogInitialize", "libSceMsgDialog"),
    ("sceMsgDialogOpen", "libSceMsgDialog"),
    ("sceMsgDialogUpdateStatus", "libSceMsgDialog"),
    ("sceMsgDialogClose", "libSceMsgDialog"),
    ("sceImeDialogInit", "libSceImeDialog"),
    ("sceImeDialogGetStatus", "libSceImeDialog"),
    ("sceSysUtilSendSystemNotificationWithText", "libSceSysUtil"),
    ("sceAppContentInitialize", "libSceAppContent"),
    ("sceCommonDialogInitialize", "libSceCommonDialog"),
    ("sceNpManagerGetNpId", "libSceNpManager"),
    ("sceNpScoreInit", "libSceNpScore"),
    ("sceGnmSubmitCommandBuffers", "libSceGnmDriver"),
    ("sceGnmSubmitAndFlipCommandBuffers", "libSceGnmDriver"),
    ("sceAjmInitialize", "libSceAjm"),
];

/// Вычислить NID по имени функции: SHA-1, затем байты 0..4 в обратном
/// порядке (схема Sony).
pub fn nid_from_name(name: &str) -> u32 {
    let digest = sha1(name.as_bytes());
    u32::from_le_bytes([digest[3], digest[2], digest[1], digest[0]])
}

/// Простая реализация SHA-1 (нужна только для NID).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | (!b & d), 0x5A827999)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// Таблица NID -> имя, загружаемая во время выполнения.
#[derive(Default, Debug, Clone)]
pub struct NidTable {
    pub map: BTreeMap<u32, String>,
}

impl NidTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Загрузка из простого JSON вида `{"0x1234abcd": "sceFoo", ...}`
    /// или из списка `"sceFoo": ["0x1234abcd"]`.
    pub fn load_json(&mut self, text: &str) -> usize {
        let mut added = 0;
        let mut chars = text.char_indices().peekable();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // ищем пары "key": "value" без полноценного парсера
            if bytes[i] == b'"' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += 1;
                }
                let key = &text[start..j];
                let mut k = j + 1;
                while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b':' {
                    k += 1;
                    while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                        k += 1;
                    }
                    if k < bytes.len() && bytes[k] == b'"' {
                        let vs = k + 1;
                        let mut ve = vs;
                        while ve < bytes.len() && bytes[ve] != b'"' {
                            ve += 1;
                        }
                        let val = &text[vs..ve];
                        if let Ok(nid) = parse_hex_u32(key) {
                            self.map.insert(nid, val.to_string());
                            added += 1;
                        } else if let Ok(nid) = parse_hex_u32(val) {
                            self.map.insert(nid, key.to_string());
                            added += 1;
                        }
                        i = ve + 1;
                        continue;
                    }
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
        let _ = &mut chars;
        added
    }

    pub fn lookup(&self, nid: u32) -> Option<&str> {
        self.map.get(&nid).map(|s| s.as_str())
    }

    /// Дополнить таблицу вычисленными NID для известных имён.
    pub fn populate_known(&mut self) {
        for (name, _) in KNOWN_EXPORTS {
            self.map.entry(nid_from_name(name)).or_insert_with(|| name.to_string());
        }
    }
}

fn parse_hex_u32(s: &str) -> Result<u32, ()> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).map_err(|_| ())
}

/// Модуль, которому принадлежит функция (по префиксу имени).
pub fn module_of(name: &str) -> &'static str {
    for (n, m) in KNOWN_EXPORTS {
        if *n == name {
            return m;
        }
    }
    if name.starts_with("sce") {
        let prefixes: &[(&str, &str)] = &[
            ("sceKernel", "libkernel"),
            ("scePthread", "libkernel"),
            ("sceSysmodule", "libSceSysmodule"),
            ("sceUserService", "libSceUserService"),
            ("sceSystemService", "libSceSystemService"),
            ("scePad", "libScePad"),
            ("sceVideoOut", "libSceVideoOut"),
            ("sceAudio", "libSceAudioOut"),
            ("sceRtc", "libSceRtc"),
            ("sceSaveData", "libSceSaveData"),
            ("sceNet", "libSceNet"),
            ("sceHttp", "libSceHttp"),
            ("sceNp", "libSceNpManager"),
            ("sceMsgDialog", "libSceMsgDialog"),
            ("sceIme", "libSceImeDialog"),
            ("sceGnm", "libSceGnmDriver"),
            ("sceAjm", "libSceAjm"),
        ];
        for (p, m) in prefixes {
            if name.starts_with(p) {
                return m;
            }
        }
        return "libSceSysCore";
    }
    if is_libc_name(name) {
        return "libSceLibcInternal";
    }
    "неизвестный"
}

fn is_libc_name(name: &str) -> bool {
    const LIBC: &[&str] = &[
        "malloc", "free", "calloc", "realloc", "memcpy", "memmove", "memset", "memcmp",
        "strlen", "strcmp", "strncmp", "strcpy", "strncpy", "strcat", "strchr", "strstr",
        "printf", "sprintf", "snprintf", "puts", "fwrite", "fopen", "fclose", "exit", "abort",
        "strtol", "strtod", "atoi", "atoi", "qsort", "bsearch", "rand", "srand", "time",
        "localtime", "mktime", "getenv", "setlocale", "pow", "sqrt", "sin", "cos", "atan2",
        "log", "exp", "floor", "ceil", "fmod", "fabs",
    ];
    LIBC.contains(&name) || name.starts_with("__cxa") || name.starts_with("_Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_empty() {
        let d = sha1(b"");
        assert_eq!(
            format!("{:02x?}", d),
            format!(
                "{:02x?}",
                [
                    0xda, 0x39, 0xa3, 0xee, 0x5e, 0x6b, 0x4b, 0x0d, 0x32, 0x55, 0xbf, 0xef, 0x95,
                    0x60, 0x18, 0x90, 0xaf, 0xd8, 0x07, 0x09
                ]
            )
        );
    }

    #[test]
    fn nid_is_stable() {
        assert_eq!(nid_from_name("sceKernelGetProcessId"), nid_from_name("sceKernelGetProcessId"));
        assert_ne!(nid_from_name("a"), nid_from_name("b"));
    }

    #[test]
    fn nid_json_roundtrip() {
        let mut t = NidTable::new();
        let n = t.load_json(r#"{"0xdeadbeef": "sceFoo", "sceBar": "0x12345678"}"#);
        assert_eq!(n, 2);
        assert_eq!(t.lookup(0xdeadbeef), Some("sceFoo"));
        assert_eq!(t.lookup(0x12345678), Some("sceBar"));
    }

    #[test]
    fn module_detection() {
        assert_eq!(module_of("scePadOpen"), "libScePad");
        assert_eq!(module_of("malloc"), "libSceLibcInternal");
        assert_eq!(module_of("__cxa_atexit"), "libSceLibcInternal");
    }
}
