<div align="center">

# LumeGX

**Do primeiro pixel à cena iluminada.**

Fork experimental do [VIctory](https://github.com/Marvisak/VIctory) para investigar como GTA VI de Xbox Series X|S executa e renderiza no Windows.

[Marcos visuais](docs/progresso/2026-11-24/README.md) · [Primeiros passos](#primeiros-passos) · [Arquitetura](docs/architecture.md) · [Guia técnico](docs/guia-victory.md)

</div>

---

## O que já apareceu na tela?

**Créditos e legendas já aparecem. A geometria do mundo também foi vista, mas a cena iluminada ainda não.** Esses resultados vêm das builds locais de diagnóstico em uma Radeon RX 5700 XT, no perfil Series X. O jogo ainda não está em estado jogável.

| Etapa | O que vimos |
| :--- | :--- |
| **01 · Introdução** | Os créditos aparecem em rosa e a legenda em português permanece legível mesmo quando o fundo está preto. |
| **02 · Mundo em 3D** | Ao exibir um alvo intermediário de 2560 × 1440, surgiram cercas, postes e fios. As cores falsas revelam a forma dos objetos, não sua iluminação final. |
| **03 · Saída em 4K** | No quadro **4896**, a GPU já recebia geometria do mundo, mas só **0,15%** dos pixels da imagem apresentada tinham algum canal acima de 5/255. A imagem já chegava escura à janela. |

**A pergunta em aberto:** a cena já entra escura no TAA2U, o ampliador usado no perfil Series X, ou perde a cor depois? Para distinguir os dois casos, precisamos identificar a textura que contém a cena iluminada e compará-la com a saída 4K **no mesmo quadro**.

O [registro de 24 de novembro de 2026](docs/progresso/2026-11-24/README.md) reúne builds, medições e hashes das capturas. Ele distingue os resultados das branches experimentais do que pode ser reproduzido a partir da `main`.

## Primeiros passos

Para colaborar, você pode compilar e executar os testes **sem os arquivos do jogo**. Use Windows x64, Git e Rust com a [toolchain fixada pelo projeto](rust-toolchain.toml).

```powershell
git clone https://github.com/gabztoo/LumeGX.git
cd LumeGX
cargo xtask install-hooks
cargo test --workspace
```

O nome do fork é **LumeGX**; o pacote Cargo e o executável continuam se chamando `victory` e `victory.exe`. Para executar o título com seus próprios arquivos, consulte os [requisitos, configuração e comandos de execução](docs/guia-victory.md#getting-started). Para investigar um quadro, veja as [opções de diagnóstico](docs/guia-victory.md#debugging).

## Trabalhar no mesmo fork

Abra uma branch por mudança e envie o PR para **`gabztoo/LumeGX:main`**. Em um PR de renderização, informe a build, o perfil de console, a GPU, o quadro observado e o que mudou na imagem. Isso permite que outra pessoa compare o mesmo estágio sem confundir loading, geometria intermediária e apresentação final.

[Arquitetura do runtime](docs/architecture.md) · [Política de arquivos e capturas](docs/asset-policy.md) · [Licença GPL-3.0-or-later](LICENSE)
