# Local Library — PS Vita / PSP

Catálogo e instalador de juegos de PS Vita y PSP. Lee una carpeta con la estructura de
memoria de la consola (`app/`, `addcont/`, `pspemu/`), muestra los juegos con su título y
tamaño real, y copia la selección a una unidad extraíble conservando esa estructura.

Incluye dos interfaces sobre el mismo motor:

- **GUI de escritorio** (egui/eframe) — binario `local-library`, aplicación principal.
- **TUI de terminal** (ratatui) — binario `local-library-tui`, útil por SSH o sin escritorio.

## AppImage

La AppImage lista para usar está en:

```
dist/LocalLibrary-0.1.0-x86_64.AppImage
```

```bash
chmod +x dist/LocalLibrary-0.1.0-x86_64.AppImage
./dist/LocalLibrary-0.1.0-x86_64.AppImage
```

Comprobaciones rápidas:

```bash
./dist/LocalLibrary-0.1.0-x86_64.AppImage --version
./dist/LocalLibrary-0.1.0-x86_64.AppImage --help
```

Si el sistema no tiene FUSE 2, se puede ejecutar extrayéndola:

```bash
APPIMAGE_EXTRACT_AND_RUN=1 ./dist/LocalLibrary-0.1.0-x86_64.AppImage
```

La AppImage fue empaquetada con el binario compilado en Debian 13 para bajar el requisito
de glibc: necesita **glibc 2.35 o superior** (Ubuntu 22.04+, Debian 12+, Fedora 36+,
Bazzite). Las bibliotecas gráficas (OpenGL/EGL, Wayland/X11, xkbcommon y los drivers de
GPU) se usan del sistema anfitrión, como es habitual en AppImage.

## Uso de la GUI

```bash
cargo run --release
```

1. Elige la carpeta de origen con **Examinar…** o escríbela y pulsa **Escanear**.
2. Marca los juegos que quieras (hay buscador, **Todos** y **Ninguno**). Cada juego muestra
   su código junto al nombre; los que ya están en la unidad llevan **✓ INSTALADO** y
   **Todos** los omite (se pueden marcar a mano para reinstalarlos).
3. Elige la unidad de destino en la lista de unidades detectadas o con **Examinar…**.
   Al elegirla se revisan sus carpetas y los juegos que ya están en ella aparecen marcados
   como **✓ INSTALADO** en el catálogo. Antes de instalar se muestra la lista de carpetas
   de destino de cada juego seleccionado.
4. Pulsa **Instalar** y observa el progreso. Puedes cancelar en cualquier momento.

El panel derecho muestra el espacio libre, el espacio necesario, los juegos ya instalados,
los de la selección que se sobrescribirán y el resultado de la copia. El botón **Revisar**
vuelve a comprobar la unidad sin cambiar la selección.

### Copiar juegos de la unidad a la librería

Al elegir la unidad, el panel derecho añade **En la unidad, fuera de tu librería**: una lista
con los juegos que están en la unidad pero no en la carpeta de origen, con su código y tamaño.
Se marcan todos por defecto; desmarca los que no quieras (o usa **Deseleccionar todos** /
**Seleccionar todos**, que alterna según el estado) y pulsa **Copiar N a mi librería**.
Los juegos se copian a la librería local con la misma estructura (`app/`, `addcont/`,
`pspemu/`), se comprueba el espacio libre y al terminar la librería se vuelve a escanear para
que aparezcan en el catálogo. Esta lista requiere haber escaneado antes la carpeta de origen.

## Uso de la TUI

```bash
cargo run --release --bin local-library-tui
```

| Tecla | Acción |
|---|---|
| `↑` `↓` / `k` `j` | Mover el cursor |
| `Espacio` | Marcar o desmarcar |
| `A` / `N` | Seleccionar todos los visibles / ninguno |
| `/` | Buscar por título o ID |
| `E` / `O` | Explorar / escribir la ruta de origen |
| `R` | Reescanear y actualizar unidades |
| `U` | Elegir una unidad montada |
| `B` / `T` | Explorar / escribir la ruta de destino |
| `V` | Ver las carpetas de destino de la selección |
| `Enter` | Instalar |
| `C` | Cancelar la instalación |
| `Q` / `Ctrl+C` | Salir |

En el explorador: **Enter** abre, **Backspace** sube, **S** elige la carpeta actual,
**H** inicio, **M** `/run/media`, **R** raíz, **Ctrl+H** ocultas y **Esc** cancela.

## Estructura esperada

- `app/` — juegos de PS Vita (`app/<TITLE_ID>/`)
- `addcont/` — DLC (`addcont/<TITLE_ID>/`, se copia junto a su juego)
- `pspemu/` — PSP (`pspemu/ISO/*.iso|.cso`, `pspemu/PSP/GAME/*/EBOOT.PBP`)

La instalación copia a la raíz de la unidad destino manteniendo `app/`, `addcont/` y
`pspemu/`, lista para la consola.

## Títulos, tamaños y seguridad

- PS Vita: `TITLE` de `app/<TITLE_ID>/sce_sys/param.sfo` (si falta, se usa el TITLE_ID).
- PSP ISO: `TITLE` de `PSP_GAME/PARAM.SFO` dentro del ISO9660; para `.cso` o ISOs sin esa
  entrada se usa el identificador de volumen o el nombre del archivo.
- PSP homebrew: `TITLE` de `EBOOT.PBP`.
- El tamaño incluye el DLC y se comprueba el espacio libre antes de empezar (16 MiB de
  margen).
- Cada archivo se copia a `*.part` y se renombra al terminar: cancelar no deja archivos
  incompletos.
- Avisa si el destino ya contiene algún juego y no borra nada que no esté sobrescribiendo.

## Compilar

```bash
cargo build --release            # GUI (local-library) y TUI (local-library-tui)
cargo test                       # 30 pruebas
cargo clippy --all-targets
```

## Generar la AppImage

Dentro del contenedor `rust-gui` (Fedora 44):

```bash
distrobox enter rust-gui
sudo dnf install -y patchelf file
cd "/run/media/system/assets/proyectos internos/localLibrary"
./packaging/build-appimage.sh
```

El script descarga `linuxdeploy` y `appimagetool` en `target/appimage-tools`, genera el
AppDir y escribe `dist/LocalLibrary-<versión>-x86_64.AppImage`. Para compilar el binario
con un requisito de glibc más bajo, se compiló en el contenedor Debian y se empaquetó con:

```bash
distrobox enter debian -- bash -lc \
  'cd "/run/media/system/assets/proyectos internos/localLibrary" && \
   CARGO_TARGET_DIR=target-debian cargo build --release --bin local-library'

distrobox enter rust-gui -- bash -lc \
  'cd "/run/media/system/assets/proyectos internos/localLibrary" && \
   BIN=target-debian/release/local-library ./packaging/build-appimage.sh'
```

## Estructura del proyecto

```
src/lib.rs                     núcleo: catálogo, unidades, explorador e instalación
src/main.rs                    GUI egui
src/bin/local-library-tui.rs   TUI ratatui
src/catalog.rs                 escaneo y lectura de param.sfo / PBP / ISO9660
src/install.rs                 planes, copia, progreso y cancelación
src/drives.rs                  detección de unidades montadas
src/browser.rs                 explorador de carpetas
assets/                        icono y lanzador .desktop
packaging/build-appimage.sh    empaquetado AppImage
```
