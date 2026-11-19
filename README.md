# ps4emu — экспериментальный эмулятор PlayStation 4 на Rust

Эмулятор консоли PlayStation 4 (Orbis OS на базе FreeBSD 9, процессор AMD Jaguar,
x86-64, GPU GCN) на чистом Rust: **без внешних зависимостей**, только стандартная
библиотека. Проект учебно-исследовательский: он честно повторяет архитектуру
консоли — от загрузчика ELF/SELF и интерпретатора x86-64 до системных вызовов
Orbis, HLE-функций системных библиотек, программного «видеовыхода», отладчика,
снимков состояния и разбора PKG-контейнеров.

```
$ ps4emu run tests/guests/hello.elf --trace
[ps4emu] загружено "hello.elf": точка входа 0x401000, модулей 1, импортов 0
Hello from PS4 guest!
[ps4emu] процесс завершён с кодом 0
```

## Чем это НЕ является

Это **не** эмулятор, на котором запускаются коммерческие игры. Реализована
инфраструктура и ядро (CPU, память, syscalls, HLE, загрузчик, отладчик), а не
полная модель консоли: нет JIT, нет реального GNM/Vulkan, нет полной эмуляции
freebsd-совместимых потоков, нет шифрования SELF/PKG. Подробный разбор ограничений —
в разделе [«Ограничения»](#ограничения).

## Возможности

### Процессор (AMD Jaguar, x86-64)

`src/cpu.rs` — интерпретатор с точной моделью флагов и полноценной адресацией.

* 16 регистров общего назначения, RIP, RFLAGS, FS/GS base, XMM0–XMM15, x87 (8 регистров).
* ModRM/SIB, все варианты дисплейсментов, REX-префиксы, 16- и 32-битные операнды,
  сегментные префиксы `fs:`/`gs:`.
* Арифметика и логика (`add/adc/sub/sbb/and/or/xor/cmp/test`), `inc/dec/neg/not`,
  `imul/idiv/mul/div` (включая 128-битные формы), сдвиги и вращения,
  `shld/shrd`, `bt/bts/btr/btc`, `bsf/bsr`, `popcnt`, `lzcnt/tzcnt`.
* Пересылки: `mov/movzx/movsx/movsxd/lea/xchg/cmovcc/xlatb`, `push/pop`, `enter/leave`.
* Строковые операции с `rep`/`repne`: `movs/stos/lods/cmps/scas`.
* Переходы и вызовы: короткие и длинные `jmp/jcc/call/ret`, `jrcxz`.
* SSE (SSE…SSE4.1/SSE4.2 в объёме, нужном для тестов): арифметика `scalar/packed`,
  `movups/movaps/movd/movq`, `pxor/pand/por`, `shufps`, сравнения, конверсии.
* x87: `fld/fst/fstp`, арифметика, сравнения, `fnstenv/fnstsw/fxch`, управление словом.
* `syscall`, `int`, `hlt`, `ud2`, `cpuid` (паспорт Jaguar: SSE2/SSE3/SSSE3/SSE4.1,
  без AVX/BMI — как на реальной консоли), `rdtsc`, `rdrand`.
* Точные исключения: `CpuError` с адресом, кодом и дампом инструкции.

### Память

`src/mem.rs`

* Плоское 48-битное адресное пространство, регионы с правами `r/w/x`.
* Два бэкенда: линейный (малые регионы) и разреженный по страницам 4 КиБ
  (большие, например 512 МиБ основной памяти).
* `read/write` разных размеров, `read_partial`, `write_cstr`, `cstr`, `hexdump`,
  `map/unmap/protect`, поиск региона по адресу, статистика чтений/записей/ошибок.
* Наблюдательные точки (`watchpoints`) для отладчика.

### Загрузчик ELF / SELF

`src/elf.rs`, `src/emulator.rs`

* ELF64 `ET_EXEC` и `ET_DYN` (PIE): загрузка `PT_LOAD` с учётом смещений страниц,
  объединение прав на границах сегментов, `PT_TLS`, `PT_PHDR`.
* Релокации `R_X86_64_64/PC32/PLT32/GLOB_DAT/JUMP_SLOT/RELATIVE/32/32S/DTPMOD64/
  DTPOFF64/TPOFF64`.
* Импорты: символы вида NID (16 hex-символов) разрешаются через базу NID; каждой
  известной функции выдаётся собственная заглушка в регионе `hle-stubs`, вызов
  заглушки перехватывается и обслуживается HLE.
* SELF/контейнер: извлечение вложенного ELF, определение формата по заголовку.
* Начальный стек в стиле System V: `argc/argv/envp/auxv` (`AT_PAGESZ=0x4000`,
  `AT_ENTRY`, `AT_PHDR`, `AT_RANDOM`, `AT_EXECFN`, …), выравнивание по 16 байт.
* TLS: статический блок `PT_TLS`, указатель потока, TCB.

### Системные вызовы Orbis OS (FreeBSD 9) и Linux

`src/sys.rs` — 121 номер вызова в таблице, включая:

* файлы: `open/openat/close/read/write/readv/writev/lseek/fstat/fstatat/stat/
  getdents/access/mkdir/unlink/rename/dup/dup2/pipe/ioctl/fcntl/fsync`;
* память: `mmap/mprotect/munmap/madvise/mremap/brk/sbrk`;
* процессы и потоки: `getpid/getppid/gettid/thr_self/thr_exit/thr_new/
  _umtx_op/clone3/set_tid_address`+`set_robust_list`, `wait4`, `kill`;
* время: `clock_gettime/gettimeofday/nanosleep/getrusage/sysconf`;
* сети: `socket/connect/bind/listen/accept/accept4/sendto/recvfrom/setsockopt/
  getsockopt/getsockname/getpeername`;
* прочее: `uname/getcwd/chdir/setuid/setgid/sigaction/sigprocmask/sigaltstack/
  rseq/prlimit64/getrandom/issetugid/getoslevel/cpuset_*`.

VFS гостя: файлы живут в памяти (`BTreeMap`), запись через дескриптор синхронизируется
с содержимым, при `--out` созданные файлы выгружаются на диск. Есть каталоги,
устройства (`/dev/urandom`, `stdin/stdout/stderr`) и сокеты-заглушки.

Режим `--linux` переключает ABI: номера вызовов транслируются из Linux в Orbis,
ошибки возвращаются как `-errno` (в Orbis — код + флаг CF), что позволяет запускать
обычные статические программы glibc.

### HLE системных библиотек PS4

`src/hle.rs` — 55 реализованных функций (имена — экспорты `libSce*`):

* libc-подобные: `malloc/free/calloc/realloc/memalign/posix_memalign/memcpy/
  memmove/memset/memcmp/strlen/strcmp/strncmp/strcpy/strncpy/strcat/strchr/
  strrchr/strstr/strdup/strtol/atoi`;
* вывод и varargs: `printf/sprintf/snprintf/vsnprintf/fprintf/fputs/fwrite/
  puts/putchar` (разбор `va_list` по SysV ABI: `gp_offset/fp_offset/overflow_arg_area`);
* ядро: `sceKernelGetProcessId`, `sceKernelGetThreadId`, `sceKernelGetProcessTime`,
  `sceKernelUsleep`, `sceKernelGettimeofday`, `sceKernelCreateEqueue`,
  `sceKernelWaitEqueue`, `sceKernelCreateSema`, `sceKernelAllocateDirectMemory`,
  `sceKernelMapDirectMemory`, `sceSysmoduleLoadModule`, `sceKernelDlsym`;
* потоки: `scePthreadCreate/Join/Detach/Exit/Self`, мьютексы, условные переменные,
  атрибуты;
* пользователь и время: `sceUserServiceGetInitialUser` (uid `0x10000001`),
  `sceRtcGetCurrentTick/GetTickResolution`;
* ввод-вывод: `scePadInit/scePadOpen/scePadRead/scePadReadState` (реальный
  DualShock 4 в память гостя), `sceVideoOut*` (атрибуты, регистрация буферов,
  `SubmitFlip`), `sceAudioOut*`;
* завершение: `exit/_exit/abort/__cxa_atexit`.

Каждый вызов логируется (с аргументами и результатом) при `--trace`, ведётся
статистика частоты вызовов; нереализованные функции попадают в отчёт
`Нереализованные вызовы`.

### Видеовыход, GPU и геймпад

`src/gpu.rs`

* Программный растеризатор: цветовой буфер RGBA8888, z-буфер, scissor,
  `clear/fill_rect/draw_line/draw_tri` (с z-тестом и интерполяцией цвета),
  поток команд `Command` + `submit`.
* Захват кадра из гостевой памяти (`sceVideoOutSubmitFlip`): RGBA → RGB,
  сохранение в **BMP** (24 бита, строки выровнены по 4 байта) и **PPM (P6)**.
* Виртуальный DualShock 4: биты кнопок (`ScePadButtonDataOffset`), стики, L2/R2,
  таймстамп; заполнение 0x78-байтовой структуры `ScePadData`; сценарии нажатий
  для тестов и автоматизации (`--pad cross+left`).

### Патчи и контейнеры

`src/patch.rs`

* Поиск по шаблону с байтами-джокерами: `"48 8B ?? 05"`.
* Патчи с ожидаемым числом совпадений (`expect N`), применение к файлу и запись
  результата.
* Текстовый формат файла патчей:
  ```
  patch "обход проверки" 48 8B ?? 05 -> 90 90 90 90
  expect 1
  ```
* Разбор заголовка PKG (`\x7fCNT`, `0x7F434E54`), таблицы записей, content id;
  открытый тестовый контейнер `FPKG` для отладки.

### Отладчик

`src/debug.rs` — интерактивный (`ps4emu debug <файл>`):

```
(emu) break 0x40101e
(emu) continue
(emu) step
(emu) regs
(emu) disas 0x40101e 6
(emu) x 0x410000 64
(emu) bt
(emu) watch 0x420000 4
(emu) info mem | info break | info sym
(emu) stats
```

Точки останова, пошаговое исполнение, дизассемблирование, дамп памяти, стек
вызовов по кадрам, наблюдательные точки, символизация (`модуль+смещение`).

### Снимки состояния

`src/snapshot.rs`

* Формат `PS4SNAP\x01`, версия, CRC-32 в конце.
* Сохраняются: GPR/XMM/x87/RIP/RFLAGS/FS/GS, все регионы памяти (только
  «непустые» страницы, сжатие нулевых серий), куча, mmap, VFS, открытые файлы,
  модули, таблица заглушек, TLS, точки останова.
* Снимок 512-МиБ адресного пространства с программой занимает десятки килобайт.
* Продолжение исполнения после загрузки снимка (в том числе из CLI).

### Инструменты командной строки

```
ps4emu run <elf|eboot.bin> [--steps N] [--trace] [--trace-cpu] [--linux]
                           [--pad СЦЕНАРИЙ] [--screenshot ФАЙЛ] [--out DIR]
ps4emu info <файл>          — формат, точки входа, сегменты, TLS
ps4emu symbols <файл>       — символы, импорты, NID
ps4emu disas <файл> [n]     — дизассемблер (с точными длинами инструкций)
ps4emu nid <имя|nid>        — вычислить NID (первые 4 байта SHA-1 в обратном порядке)
ps4emu mem <файл>           — карта памяти после загрузки
ps4emu bench <файл> [n]     — скорость интерпретации (млн инстр./с)
ps4emu modules | hle        — база модулей и список HLE-функций
ps4emu screenshot <elf> <bmp|ppm> [--steps N] [--pad …]
ps4emu snapshot save|load|info …
ps4emu patch <файл> <патч.txt> [--out ФАЙЛ]
ps4emu pkg <файл.pkg>       — список файлов в PKG/FPKG
ps4emu debug <файл>         — отладчик
ps4emu self-test            — самопроверка ядра
```

### База NID и модулей

`src/oracle.rs` — 62 системных модуля и ~170 известных экспортов; NID вычисляется
как первые четыре байта SHA-1 от имени функции (с обратным порядком байт), что
совпадает с реальными значениями (например, `sceKernelGetProcessId = eed71b46`).
Поддерживается загрузка дополнительной таблицы `--nids table.json`.

## Сборка и запуск

Нужен только Rust (edition 2021), внешних крейтов нет:

```
cargo build --release
cargo test                 # 88 тестов: 25 модульных + 42 CPU + 21 интеграционный
cargo run --release -- run tests/guests/hello.elf --trace
```

Сборка гостевых примеров (нужен `gcc` и binutils для тестового окружения):

```
gcc -static -nostdlib -o /tmp/hello.elf tests/guests/hello.S
gcc -static -nostdlib -o /tmp/exit0.elf tests/guests/exit0.S
gcc -static -nostdlib -o /tmp/loop.elf tests/guests/loop.S
gcc -shared -nostdlib -fPIC -o /tmp/video.so tests/guests/video.S
ps4emu run    /tmp/hello.elf
ps4emu screenshot /tmp/video.so /tmp/frame.bmp
```

`tests/guests/video.S` рисует градиент 64×64 и представляет кадр через
`sceVideoOutSubmitFlip`; `ps4emu screenshot` снимает результат.

## Архитектура

| Файл | Назначение |
| --- | --- |
| `src/mem.rs` | виртуальная память, регионы, права, наблюдательные точки |
| `src/cpu.rs` | интерпретатор x86-64 + SSE/x87 + cpuid Jaguar |
| `src/elf.rs` | разбор ELF64, SELF, символов и релокаций |
| `src/sys.rs` | системные вызовы Orbis/Linux, VFS, файлы, потоки, время |
| `src/hle.rs` | реализация функций `libSce*` и libc |
| `src/oracle.rs` | база NID ↔ имя, модули, хеш имён |
| `src/emulator.rs` | загрузка образа, стек, TLS, главный цикл, статистика |
| `src/gpu.rs` | растеризатор, кадры BMP/PPM, виртуальный геймпад |
| `src/patch.rs` | шаблоны, патчи, PKG/FPKG |
| `src/debug.rs` | отладчик и его команды |
| `src/snapshot.rs` | сериализация состояния и CRC-32 |
| `src/trace.rs` | трассировщик, дизассемблер, символизация |
| `src/config.rs` | конфигурация запуска |
| `src/main.rs` | CLI |

Поток исполнения: `load` (ELF → память → релокации → заглушки) → `setup_stack`
(аргументы, auxv, TLS) → `run` (цикл: HLE-заглушка / syscall / инструкция) →
отчёт (статистика, стек при сбое, файлы гостя на диск).

### Диагностика

Переменные окружения: `PS4EMU_VERBOSE` (дублировать лог syscalls в stderr),
`PS4EMU_BT`, `PS4EMU_BT_SYS` (стек вызовов при ошибке/на записи в fd 2),
`PS4EMU_TRACE_TAIL`, `PS4EMU_SANITY` (проверка аномалий RIP), `PS4EMU_X87DBG`.

## Тесты

* `src/*` — 25 модульных тестов (память, NID, дизассемблер, GPU, патчи, снимки…);
* `tests/cpu_tests.rs` — 42 теста процессора: арифметика, флаги, строковые
  операции, SSE, x87, cpuid, адресация, точные длины инструкций;
* `tests/emulator_tests.rs` — 21 интеграционный тест: сборка гостевых ELF
  «на лету» (`tests/common/mod.rs`), коды выхода, `write`, `mmap`, VFS,
  `printf` через HLE, `malloc/memcpy/free`, стек `argc/argv/envp/auxv`,
  errno в Linux ABI, `ENOSYS`, снимки, точки останова, BMP-скриншот, геймпад,
  `sceUserServiceGetInitialUser`, путь «гость отрисовал → SubmitFlip → кадр»,
  имена импортов из `.dynsym`.

## Ограничения

Честный список того, что не работает или работает частично:

* **Коммерческие игры не запускаются.** Нет шифрования/подписи SELF, нет
  загрузки PRX-модулей с реальным разрешением импортов, нет GNM/GCN.
* **CPU**: интерпретатор, а не JIT — скорость порядка единиц миллионов
  инструкций в секунду (`ps4emu bench`), тогда как Jaguar исполняет миллиарды.
  Покрытие инструкций неполное: нет AVX/FMA (их нет и на Jaguar), нет
  `xsave/xrstor`, редких системных форм, полноценного `fxsave`.
* **Многопоточность**: потоки моделируются последовательно, без вытеснения;
  `clone3`/`thr_new` возвращают успех, но реального параллелизма нет,
  синхронизация — заглушки.
* **Память**: единая модель без реальных страничных отказов и copy-on-write;
  нет `mprotect`-эффектов на уровне MMU (только проверки в эмуляторе).
* **GPU**: программный растеризатор и захват кадрового буфера; шейдеры,
  текстуры, GNM-команды и Vulkan-вывод не реализованы. `sceVideoOutSubmitFlip`
  лишь помечает буфер, который затем снимается в BMP/PPM.
* **Звук**: `sceAudioOut*` — заглушки, микширования нет.
* **Сеть**: сокеты существуют как объекты, но реального стека нет (кроме
  виртуального `/dev/urandom` и локальной VFS).
* **PKG/SELF**: разбор заголовков и «открытого» контейнера; настоящее
  шифрование PKG и подписи не поддерживаются.
* **HLE**: 55 функций — покрывают libc-подобный минимум и часть `libSce*`;
  поведение многих (очереди, семафоры, модули) упрощено.
* **glibc-гости**: статические программы доходят до `main`, но падают в
  `__libc_malloc` на проверке арен аллокатора — известная незакрытая проблема
  (не varargs): см. `PS4EMU_BT_SYS=1`.
* **Отладчик**: нет условных точек останова, обратного исполнения и пошагового
  «шага с обходом» на уровне исходников.

## Лицензия

Учебный проект; используйте на свой страх и риск. PlayStation — торговая марка
Sony Interactive Entertainment; проект не связан с Sony и не содержит её кода.
