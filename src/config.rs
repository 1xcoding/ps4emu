//! Конфигурация эмулятора.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Файл гостевой программы (ELF/SELF).
    pub elf: Option<PathBuf>,
    /// Каталог с nids.json (база NID -> имя).
    pub nids: Option<PathBuf>,
    /// Предел выполненных инструкций (0 = без предела).
    pub max_steps: u64,
    /// Трассировка системных вызовов и HLE.
    pub trace: bool,
    /// Трассировка инструкций (очень медленно).
    pub trace_cpu: bool,
    /// Выводить дамп памяти при ошибке.
    pub dump_on_fault: bool,
    /// Каталог для файлов, созданных гостем.
    pub out_dir: PathBuf,
    /// Виртуальный путь к основной программе внутри VFS.
    pub app0: String,
    /// Аргументы гостевой программы.
    pub args: Vec<String>,
    /// Переменные окружения.
    pub env: Vec<(String, String)>,
    /// Размер основной памяти (по умолчанию 512 МиБ).
    pub memory_size: u64,
    /// Включить окно экрана (GPU).
    pub window: bool,
    /// Ширина и высота экрана.
    pub width: u32,
    pub height: u32,
    /// Пауза между кадрами (имитация vblank).
    pub vsync: bool,
    /// Детерминированное время.
    pub fixed_time: bool,
    /// Логирование в файл.
    pub log_file: Option<PathBuf>,
    /// Показывать статистику по завершении.
    pub stats: bool,
    /// Сценарий нажатий геймпада, например `cross+left` (см. `gpu::pad`).
    pub pad_script: Option<String>,
    /// Сохранить снятый кадр в этот файл (`.bmp` или `.ppm`).
    pub screenshot: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            elf: None,
            nids: None,
            max_steps: 0,
            trace: false,
            trace_cpu: false,
            dump_on_fault: true,
            out_dir: PathBuf::from("out"),
            app0: "/app0/eboot.bin".into(),
            args: Vec::new(),
            env: Vec::new(),
            memory_size: 512 * 1024 * 1024,
            window: false,
            width: 1280,
            height: 720,
            vsync: false,
            fixed_time: false,
            log_file: None,
            stats: true,
            pad_script: None,
            screenshot: None,
        }
    }
}
