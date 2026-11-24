# LumeGX — créditos, legendas e primeira geometria do mundo

**Marco de 24 de novembro de 2026 · Windows, Radeon RX 5700 XT, perfil Series X.** A investigação local sobre a base VIctory conseguiu mostrar elementos 2D da introdução e visualizar geometria do mundo em um alvo intermediário. A cena 3D iluminada ainda não aparece corretamente na saída normal. Este registro preserva resultados de várias sessões; não representa uma versão jogável da `main` do fork.

## Evidências visuais originais

| Captura | O que a imagem mostra | Indicador da janela no instante da captura |
|---|---|---|
| 1 · Créditos | Crédito de direção de desenvolvimento em rosa sobre fundo preto. | 2800,0 ms/frame; 541 draws/frame |
| 2 · Legenda em português | Uma linha de diálogo é legível em branco mesmo sem a cena de fundo. | 2633,3 ms/frame; 594 draws/frame |
| 3 · Geometria | Cerca, postes, fios e estruturas aparecem em cores falsas na visualização de diagnóstico. Há geometria reconhecível, não uma cena com materiais e iluminação finais. | 3259,8 ms/frame; 2816 draws/frame |

Os três PNGs foram preservados como enviados. As métricas são leituras instantâneas da barra da janela, não médias de desempenho. Não há identificação de commit ou de quadro embutida que permita atribuir essas três imagens à mesma execução.

## Marcos técnicos e como reencontrar as medições

| Referência local | Resultado verificável |
|---|---|
| `victory-show`, diagnóstico anterior sobre `9b76bcd` | Exibir diretamente o alvo `2560x1440-f28` revelou geometria em cores falsas. É evidência de geometria intermediária, não de cor iluminada. |
| `victory-combo` · `0ce1c17` · `20261124-120747-combo` | O relatório do título registrou `Prestream` e 4040 pedidos. A última janela chegou a 10,59 s de preparação por quadro antes do aborto pelo watchdog. |
| `20261124-122407-combo` → `20261124-124549-combo` | O primeiro ensaio expôs conflito de leitura/escrita do CSV do monitor. A correção passou por teste concorrente e parada de processo fictício. O perfil `diag` passou a ser selecionado e encerrado pelo caminho exato. A captura seguinte atingiu 230 s e recolheu sete amostras de pilhas, sem reproduzir a carga pesada. |
| `0ce1c17` · `20261124-125230-combo` | A saída ficou preta no quadro 2105; no 2109 apareceram os binds de TAA2 Resolve e ColorDownsample. A carga chegou a 343 draws/frame. Bind não prova que o shader recebeu ou produziu pixels válidos. |
| `victory-show` · `84cd499` · `20261124-130855-show` | Foi implementada captura única por evento de geometria do mundo, com saída nativa, intermediários em DDS e manifesto de uso de recursos. Dez testes passaram, incluindo captura conjunta de saída e HDR em teste de GPU. O marcador disparou no quadro **4896** em jogo real. |

### Quadro 4896: a imagem já chegava escura à janela

Na textura apresentada, só **12.050 de 8.294.400 pixels** tinham algum canal RGB acima de 5/255. No alvo intermediário `2560x1440-f27` salvo nesse quadro, os três canais RGB eram zero. O manifesto também listou um alvo `f10`, mas a captura encerrou por perda de amostra do monitor antes de salvar seus pixels. Não foi demonstrado que `f27` ou `f10` continha a cena iluminada.

Conforme a política de assets do repositório, os PNGs ficam no checkpoint local `checkpoints/2026-11-24-lumegx/imagens/`. Identificadores SHA-256 para verificar os originais:

```text
03f0550dc911c8750298c60cc67daec411bf198440c8bf29499358e084a3ba78  01-creditos.png
faac94c3a8824eb4b8e5901b2821dad7f33c496fd4130e464ce7bf4742c4f766  02-legenda.png
07de40790b5f46ea480327354df84125dd55dfdde6c0360091eb886e8806e756  03-geometria.png
59186d209ac0fe0be31115d7c710a386bc548898dba975c714fec3b68f6ba7ed  04-quadro-4896.png
```

Evidências locais completas: `wt/victory-data/logs/20261124-130855-show/world-capture/` (manifesto, dois DDS e prévia). SHA-256 do executável `show/diag`: `2a1cbe44b054a9120c7706dbe64b2847cc5568e0dc472a0d7436bc6cbb9c9129`.

**Próximo critério de avanço:** identificar pelo produtor e consumidor a textura que leva a cor da cena ao resolve; salvar seus pixels com prioridade e compará-los com a saída 4K do **mesmo quadro**. Isso distingue perda de cor anterior ao resolve de falha no resolve ou na composição final. Não combinar recursos de duas sessões como se fossem um único quadro.
