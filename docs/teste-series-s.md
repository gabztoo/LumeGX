# Teste: perfil Series S

Guia para rodar o **próprio dump** de GTA VI (Xbox Series, 1.0.2.1) no perfil **Series S**.
Ideal para PCs com **32 GB de RAM** ou mais: o Windows limita a memória compartilhada da GPU a metade da RAM,
e com 16 GB o jogo estoura (~8 GB) ao entrar no mundo.

## Flags usadas no teste (todas opt-in)
- `VICTORY_RGB9E5_AS_R11G11B10=1`: o Series S pede um alvo HDR 1920x1080 `R9G9B9E5_SHAREDEXP` como render target/UAV.
  O PC não aceita; ele vira `R11G11B10_FLOAT` (mesmo tamanho por pixel).
- `VICTORY_PRESENT_PLANES=1`: o Series S apresenta 2 planos por quadro: cena 3D 1920x1080 (plane 0) e interface 3840x2160
  (plane 1, menu/HUD/legendas). O hardware do Xbox junta os dois; o LumeGX agora compõe (interface por cima, alpha
  premultiplicado). Antes só o plane 0 aparecia, e o menu ficava preto. Depuração: `VICTORY_PRESENT_PLANES_BLEND=over|opaque|ignore-alpha`.
- `VICTORY_OWN_UPSCALER=1` (opcional): mantém o upscaler próprio do jogo em vez do FSR do Xbox.
- O script usa o build **diag** (`--features diagnostics`) para ter logs completos.

## Requisitos
- Windows 10/11 e GPU DX12 (com ou sem mesh shaders).
- Rust (o `rust-toolchain.toml` escolhe a versão; instale via https://rustup.rs).
- O seu dump do jogo, **fora** do repositório. Nada do jogo entra no Git.
- Opcional e recomendado: AMD FidelityFX SDK para PC (`amd_fidelityfx_upscaler_dx12.dll`, licença MIT, assinado pela AMD),
  de GPUOpen `FidelityFX-SDK`, `Kits/FidelityFX/signedbin`. Coloque a DLL numa pasta fora do repositório.

## Como rodar
```powershell
git clone https://github.com/gabztoo/LumeGX.git
cd LumeGX
copy victory.example.toml victory.toml   # opcional: ajuste os caminhos
.\scripts\teste-series-s.ps1 -Game 'D:\seu\dump\game_scarlett_master.exe' -FidelityFx 'D:\ffx'
```
O primeiro build leva alguns minutos. Na tela inicial, espere uns 30 s e aperte um botão do controle.
O loading demora. No mundo, o fps é baixo (0,1–0,3 fps): as cópias CPU<->GPU ainda não foram otimizadas. Texturas e objetos
aparecem incompletos (streaming lento). Com 16 GB de RAM ocorre falta de memória (`0x8007000E`) aos ~5 min; com 32 GB o teto dobra.

## O que reportar
- Até onde chegou (menu, loading, mundo, cutscene) e prints.
- A pasta de logs que o script mostra no fim (`stderr.log`, `stdout.log`).
- RAM, GPU e se apareceu `0x8007000E` (falta de memória).
