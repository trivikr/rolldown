use super::asset::Asset;
use crate::bundler::{
  bitset::BitSet,
  chunk::{chunk::Chunk, ChunkId, ChunksVec},
  graph::graph::Graph,
  module::module::Module,
  options::{
    normalized_input_options::NormalizedInputOptions,
    normalized_output_options::NormalizedOutputOptions,
  },
};
use anyhow::Ok;
use index_vec::IndexVec;
use rolldown_common::{ImportKind, ModuleId};
use rustc_hash::FxHashMap;

pub struct Bundle<'a> {
  graph: &'a mut Graph,
  output_options: &'a NormalizedOutputOptions,
}

impl<'a> Bundle<'a> {
  pub fn new(graph: &'a mut Graph, output_options: &'a NormalizedOutputOptions) -> Self {
    Self { graph, output_options }
  }

  pub fn mark_modules_entry_bit(
    &self,
    module_id: ModuleId,
    index: usize,
    modules_entry_bit: &mut IndexVec<ModuleId, BitSet>,
  ) {
    if modules_entry_bit[module_id].has_bit(index.try_into().unwrap()) {
      return;
    }
    modules_entry_bit[module_id].set_bit(index.try_into().unwrap());
    if let Module::Normal(m) = &self.graph.modules[module_id] {
      m.import_records.iter().for_each(|i| {
        // because dynamic import is already as entry, so here ignore it
        if i.kind != ImportKind::DynamicImport {
          self.mark_modules_entry_bit(i.resolved_module, index, modules_entry_bit);
        }
      });
    }
  }

  pub fn generate_chunks(&self) -> (ChunksVec, IndexVec<ModuleId, Option<ChunkId>>) {
    let mut module_to_bits = index_vec::index_vec![
      BitSet::new(self.graph.entries.len().try_into().unwrap());
      self.graph.modules.len()
    ];

    let mut chunks = FxHashMap::default();
    chunks.shrink_to(self.graph.entries.len());

    for (i, (name, module_id)) in self.graph.entries.iter().enumerate() {
      let count: u32 = u32::try_from(i).unwrap();
      let mut entry_bits = BitSet::new(self.graph.entries.len().try_into().unwrap());
      entry_bits.set_bit(count);
      let c = Chunk::new(name.clone(), Some(*module_id), entry_bits.clone(), vec![]);
      chunks.insert(entry_bits, c);
    }

    self.graph.entries.iter().enumerate().for_each(|(i, (_, entry))| {
      self.mark_modules_entry_bit(*entry, i, &mut module_to_bits);
    });

    self
      .graph
      .modules
      .iter()
      .enumerate()
      // TODO avoid generate runtime module
      .skip_while(|(module_id, _)| module_id.eq(&self.graph.runtime.id)) // TODO avoid generate runtime module
      .for_each(|(_, module)| {
        let bits = &module_to_bits[module.id()];
        if let Some(chunk) = chunks.get_mut(bits) {
          chunk.modules.push(module.id());
        } else {
          // TODO share chunk name
          let len = chunks.len();
          chunks.insert(
            bits.clone(),
            Chunk::new(Some(len.to_string()), None, bits.clone(), vec![module.id()]),
          );
        }
      });

    let chunks = chunks
      .into_values()
      .map(|mut chunk| {
        chunk.modules.sort_by_key(|id| self.graph.modules[*id].exec_order());
        chunk
      })
      .collect::<ChunksVec>();

    let mut module_to_chunk: IndexVec<ModuleId, Option<ChunkId>> = index_vec::index_vec![
      None;
      self.graph.modules.len()
    ];

    // perf: this process could be done with computing chunks together
    for (i, chunk) in chunks.iter_enumerated() {
      for module_id in &chunk.modules {
        module_to_chunk[*module_id] = Some(i);
      }
    }

    (chunks, module_to_chunk)
  }

  pub fn generate(
    &mut self,
    _input_options: &'a NormalizedInputOptions,
  ) -> anyhow::Result<Vec<Asset>> {
    use rayon::prelude::*;
    let (mut chunks, module_to_chunk) = self.generate_chunks();

    chunks.iter_mut().par_bridge().for_each(|chunk| chunk.render_file_name(self.output_options));

    chunks.iter_mut().par_bridge().for_each(|chunk| {
      chunk.de_conflict(self.graph);
    });

    chunks.iter_mut().for_each(|chunk| {
      if chunk.entry_module.is_some() {
        chunk.initialize_exports(&mut self.graph.modules, &self.graph.symbols);
      }
    });

    let assets = chunks
      .iter()
      .enumerate()
      .map(|(_chunk_id, c)| {
        let content = c.render(self.graph, &module_to_chunk, &chunks).unwrap();

        Asset { file_name: c.file_name.clone().unwrap(), content }
      })
      .collect::<Vec<_>>();

    Ok(assets)
  }
}