# HI Observer

HI Observer 是一个面向 Airspy 接收机的实时射电频谱观测程序。程序通过 SoapySDR 采集 IQ 数据，经过多相滤波器组（PFB）生成频谱，同时显示瀑布图和当前频谱，并可把观测结果保存为原始二进制文件或 FITS Binary Table。

当前主要可执行程序是 `channelize`，支持 macOS、Windows 和 Linux。GitHub Releases 中的预编译包已经包含运行时所需的 SoapySDR/Airspy 动态库；从源码编译时仍需安装下述开发依赖。

## 1. 硬件与软件依赖

### 1.1 硬件

- Airspy 接收机；程序固定使用 SoapySDR 的 `airspy` 驱动。
- 可用的 USB 端口和合适的射频天线、前端。

运行前可用以下命令确认系统能够识别设备：

```bash
SoapySDRUtil --find="driver=airspy"
SoapySDRUtil --probe="driver=airspy"
```

### 1.2 通用源码依赖

- Rust stable（项目使用 Rust 2024 edition）
- SoapySDR 0.8
- SoapyAirspy 模块
- libairspy、libusb、pkg-config
- 与本仓库同级放置的 [`rsdsp`](https://github.com/astrojhgu/rsdsp) 源码

目录结构必须类似：

```text
工作目录/
├── HI_observer/
└── rsdsp/
```

准备源码：

```bash
git clone https://github.com/astrojhgu/rsdsp.git
git clone https://github.com/guotsuan/HI_observer.git
cd HI_observer
```

### 1.3 macOS

项目已在 Apple Silicon macOS 上验证。建议通过 Miniforge/Conda 安装本地库：

```bash
conda install -c conda-forge soapysdr soapysdr-module-airspy libairspy libusb pkg-config
```

安装 Rust：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
```

编译时让 `pkg-config` 找到 Conda 中的 SoapySDR：

```bash
PKG_CONFIG_PATH="$CONDA_PREFIX/lib/pkgconfig" \
RUSTFLAGS="-C link-arg=-Wl,-rpath,$CONDA_PREFIX/lib" \
cargo build --release --bin channelize
```

也可以直接下载 GitHub Releases 中的 `macos-arm64` 压缩包。解压后双击 `run_channelize.command`，或在终端运行 `bin/channelize`。发布包采用 ad-hoc 签名、未做 Apple 公证；若首次启动被 Gatekeeper 拦截，可在 Finder 中右键选择“打开”，确认程序来源后再运行。

### 1.4 Windows

普通用户建议下载 GitHub Releases 中的 `windows-x64` 压缩包。解压后可双击 `run_channelize.bat`，发布包中已经包含 SoapySDR、SoapyAirspy、libairspy、libusb 及其必要 DLL。请保持 `bin` 和 `lib` 的相对目录结构不变。

从源码原生编译需要 Rust GNU 工具链、MinGW-w64、SoapySDR 0.8、SoapyAirspy 和 Airspy/libusb 的 Windows 开发文件。相较之下，预编译包或 PothosSDR 环境更容易部署。

### 1.5 Linux

不同发行版的软件包名称可能略有区别，需安装 Rust、`pkg-config`、SoapySDR 开发包、SoapyAirspy、libairspy/libusb，以及窗口系统相关开发库。仓库中的 `shell.nix` 可作为 Nix 环境参考。

普通用户访问 Airspy 时可能需要 udev 规则。将下面内容写入 `/etc/udev/rules.d/99-airspy.rules`，并按实际用户名和用户组调整权限：

```text
SUBSYSTEM=="usb", ATTR{idVendor}=="1d50", ATTR{idProduct}=="*", MODE="0660", GROUP="plugdev"
```

然后重新加载规则并重新插拔设备：

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## 2. 编译与运行

编译主程序：

```bash
cargo build --release --bin channelize
```

典型的氢线观测启动命令：

```bash
./target/release/channelize \
  --freq 1420.405751e6 \
  --lna 5 --mix 5 --vga 5
```

也可边编译边运行：

```bash
cargo run --release --bin channelize -- \
  --freq 1420.405751e6 --lna 5 --mix 5 --vga 5
```

查看当前版本的完整参数说明：

```bash
./target/release/channelize --help
```

### 2.1 运行参数

| 参数 | 必需 | 默认值 | 含义 |
|---|---:|---:|---|
| `-f, --freq <Hz>` | 是 | — | 初始中心频率，单位 Hz |
| `-n, --nch <N>` | 否 | `512` | 频率通道数；必须是 2 的幂且不大于 8192 |
| `-t, --tap <N>` | 否 | `4` | PFB 每通道抽头数 |
| `-y <N>` | 否 | `128` | 界面瀑布图保留的频谱行数 |
| `-a <N>` | 否 | `128` | 每个输出频谱行平均的瞬时频谱数 |
| `-k <K>` | 否 | `0.9` | 下半部分实时频谱的指数平滑系数，范围 `[0, 1)`；只影响显示，不影响保存数据 |
| `--lna <dB>` | 否 | `5` | Airspy LNA 增益 |
| `--mix <dB>` | 否 | `5` | Airspy Mixer 增益 |
| `--vga <dB>` | 否 | `5` | Airspy VGA 增益 |
| `-s <MHz>` | 否 | `6` | 采样率，目前仅接受 `3` 或 `6` MHz |
| `-o, --out <FILE>` | 否 | — | 启动后立即保存到指定文件；后缀决定 `.bin` 或 FITS 格式 |
| `-r, --renderer <NAME>` | 否 | `glow` | 图形渲染器，可选 `glow` 或 `wgpu` |

一个保存 FITS 的完整示例：

```bash
./target/release/channelize \
  -f 1420.405751e6 -s 6 -n 512 -t 4 -a 128 -y 256 \
  --lna 5 --mix 5 --vga 5 \
  --out observation.fits
```

每个保存行的标称时间分辨率为：

```text
time_resolution = nch × average / (2 × sample_rate)
```

例如 `nch=512`、`average=128`、`sample_rate=6 MHz` 时约为 `5.461 ms/行`。因此默认 `-y 128` 只对应约 `0.70 s` 的缓存；脚本显示多少秒取决于数据行数和该时间分辨率，不是固定 4 秒。系统负载过高时数据队列可能丢行，FITS 中的 `TIME` 列使用实际经过时间，可反映这类间隔。

## 3. 界面操作

- 上半部分为瀑布图，横轴是 `Frequency (MHz)`，纵轴是 `Time (s)`。
- 下半部分为实时频谱，横轴是 `Frequency (MHz)`，纵轴是 `Intensity (dB)`。
- `min ch` / `max ch`：选择显示的频率通道范围。
- `zoom in`：调整下半部分频谱的纵轴显示范围。
- `Reset`：取消参考谱除法，恢复普通频谱显示。
- `Excl`：将当前频谱保存为参考谱，后续显示当前频谱与参考谱之比。
- `A/S/D`：中心频率分别增加 5/1/0.1 MHz。
- `Z/X/C`：中心频率分别减少 5/1/0.1 MHz。

界面最小尺寸为 1200 × 640 逻辑像素。底部工具栏允许换行，以保证窗口缩小时所有控件仍可见。

## 4. 观测数据存储

### 4.1 从界面开始和停止保存

1. 点击 `Select file to save`。
2. 在系统文件选择窗口中选择目录，并输入以 `.bin`、`.fits` 或 `.fit` 结尾的文件名；也可以新建文件名。
3. 确认后，文件名和识别出的格式会显示在底部状态栏。
4. 点击 `Start to save` 开始写入，按钮会变为 `Stop saving`，同时显示 `Recording`。
5. 观测结束时点击 `Stop saving`。关闭主窗口时程序也会尝试正常结束保存。

选择文件以后才会创建或覆盖它。通过界面开始保存会覆盖同名文件；命令行 `--out file.bin` 保留原有 `.bin` 追加行为，而 `--out file.fits` 会创建新的 FITS 观测文件。

### 4.2 `.bin` 格式

`.bin` 文件按时间顺序连续保存频谱行。每行包含 `nch` 个小端 `float32`，数值为平均后的线性功率，没有文件头或元数据。因此读取时必须另外知道中心频率、采样率、通道数、平均数等运行参数。

### 4.3 FITS 格式

`.fits` 和 `.fit` 保存为标准 FITS Binary Table，扩展名为 `SPECTRA`。每行包含：

- `TIME`：相对 `DATE-OBS` 的实际经过时间，单位秒；
- `FREQUENCY`：该行采集时的中心频率，单位 Hz；
- `SPECTRUM`：长度为 `NCHANS` 的线性功率数组。

表头还保存 UTC 开始/结束时间、MJD 参考时间、采样率、总带宽、通道宽度、初始中心频率、通道数、PFB 抽头数、平均数、标称时间分辨率和 LNA/MIX/VGA 增益等信息。

FITS 的行数、结束时间和 2880 字节块填充会在停止保存时写入。因此应优先使用 `Stop saving` 或正常关闭窗口；强制结束进程可能留下未完成的 FITS 文件。

## 5. Python 示例画图脚本

脚本需要 Python 3，以及：

```bash
python3 -m pip install numpy matplotlib astropy
```

两个脚本都生成与主程序相同布局的瀑布图和频谱图。`--rows` 控制瀑布图使用最近多少行，默认 128；设为 `0` 或负数会画全部数据。`--spectrum-average N` 控制下半频谱对最后 N 行在线性功率空间平均后再转为 dB，默认只使用最后 1 行。

### 5.1 读取 `.bin`

由于 `.bin` 不含元数据，必须至少给出输入文件和中心频率，并确保 `--sample-rate`、`--nch`、`--average` 与采集参数一致：

```bash
python3 scripts/plot_bin.py observation.bin \
  --center-frequency 1420.405751e6 \
  --sample-rate 6e6 --nch 512 --average 128 \
  --rows 512 --spectrum-average 32
```

保存 PNG 而不弹出窗口：

```bash
python3 scripts/plot_bin.py observation.bin \
  --center-frequency 1420.405751e6 \
  --output observation-bin.png --no-show
```

### 5.2 读取 FITS

FITS 中已有观测参数，因此通常只需指定文件名：

```bash
python3 scripts/plot_fits.py observation.fits \
  --rows 512 --spectrum-average 32
```

显示完整瀑布图，并把图保存为 PNG：

```bash
python3 scripts/plot_fits.py observation.fits \
  --rows 0 --spectrum-average 128 \
  --output observation-fits.png --no-show
```

查看脚本的全部选项：

```bash
python3 scripts/plot_bin.py --help
python3 scripts/plot_fits.py --help
```

## 6. 发布包说明

- macOS：`HI_observer-macos-arm64.zip`，适用于 Apple Silicon。
- Windows：`HI_observer-windows-x64.zip`，适用于 64 位 Windows。
- 发布压缩包不提交进 Git；`target/`、`dist/`、观测数据和 Python 缓存也由 `.gitignore` 排除。

预编译包仍需要目标机器能够访问 Airspy USB 设备。Windows 包未附硬件驱动时，请先安装 Airspy 官方驱动或 PothosSDR 运行环境。
