use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::rc::Rc;
use std::rc::Weak;

pub trait GetBlockIds<Identifier> {
    fn get_block_id(&self) -> Identifier;
    fn get_block_prev_id(&self) -> Identifier;
}

#[derive(Debug, Clone)]
enum NextNode<Data> {
    Single(Rc<RefCell<Node<Data>>>),
    Multiple(Vec<Rc<RefCell<Node<Data>>>>),
}

#[derive(Debug, Clone)]
pub struct Node<Data> {
    block: Option<Data>,
    /// Weak to avoid reference cycles with the parent's `next` list
    prev: Option<Weak<RefCell<Node<Data>>>>,
    next: Option<NextNode<Data>>,
}

#[derive(Debug, Clone)]
pub struct Chain<I, D> {
    head: Option<Rc<RefCell<Node<D>>>>,
    nodes: BTreeMap<I, Rc<RefCell<Node<D>>>>,
    /// Blocks whose parent is not known yet, keyed by parent id.
    /// Competing blocks can share the same parent, hence the Vec.
    orphans: BTreeMap<I, Vec<D>>,
    /// Blocks of losing forks, collected when `pop_head` settles a
    /// branch point. Drain with `take_discarded`.
    discarded: Vec<D>,
    genesis_identifier: I,
}

impl<D> Node<D> {
    fn add_next(&mut self, node: Rc<RefCell<Node<D>>>) {
        match &mut self.next {
            Some(next) => match next {
                NextNode::Single(next) => {
                    let nodes = vec![next.clone(), node.clone()];
                    self.next = Some(NextNode::Multiple(nodes));
                }
                NextNode::Multiple(nodes) => nodes.push(node),
            },
            None => self.next = Some(NextNode::Single(node)),
        }
    }

    fn depth(node: Rc<RefCell<Node<D>>>) -> u32 {
        match &node.borrow().next {
            Some(next) => match next {
                NextNode::Single(next) => 1 + Node::depth(Rc::clone(next)),
                NextNode::Multiple(nodes) => {
                    let mut max_depth = 0;
                    for next in nodes.iter() {
                        let depth = Node::depth(Rc::clone(next));
                        if depth > max_depth {
                            max_depth = depth;
                        }
                    }
                    1 + max_depth
                }
            },
            None => 1,
        }
    }

    // Extract all nodes recursively from the current node to the head
    fn extract_left(node: Rc<RefCell<Node<D>>>) -> Vec<Rc<RefCell<Node<D>>>> {
        let prev = node.borrow().prev.as_ref().and_then(Weak::upgrade);
        match prev {
            Some(prev) => {
                let mut nodes = Node::extract_left(prev);
                nodes.push(node);
                nodes
            }
            None => vec![node],
        }
    }

    // Extract all nodes recursively from the current node to the tails,
    // each parent before its children
    fn extract_right(node: Rc<RefCell<Node<D>>>) -> Vec<Rc<RefCell<Node<D>>>> {
        let mut all_nodes = vec![Rc::clone(&node)];
        match &node.borrow().next {
            Some(NextNode::Single(next)) => {
                all_nodes.append(&mut Node::extract_right(Rc::clone(next)));
            }
            Some(NextNode::Multiple(nodes)) => {
                for next in nodes.iter() {
                    all_nodes.append(&mut Node::extract_right(Rc::clone(next)));
                }
            }
            None => {}
        }
        all_nodes
    }

    /// Extract the tail of longest chain from the current node
    fn longest_right(node: Rc<RefCell<Node<D>>>) -> Rc<RefCell<Node<D>>> {
        match &node.borrow().next {
            Some(next) => match next {
                NextNode::Single(next) => Rc::clone(next),
                NextNode::Multiple(nodes) => {
                    let mut max_depth = 0;
                    let mut longest = Rc::clone(&node);
                    for next in nodes.iter() {
                        let depth = Node::depth(Rc::clone(next));
                        if depth > max_depth {
                            max_depth = depth;
                            longest = Rc::clone(next);
                        }
                    }
                    longest
                }
            },
            None => Rc::clone(&node),
        }
    }
}

impl<I: Ord + Copy, D: GetBlockIds<I>> Chain<I, D> {
    pub fn new(genesis_identifier: I) -> Chain<I, D> {
        Chain {
            head: None,
            orphans: BTreeMap::new(),
            nodes: BTreeMap::new(),
            discarded: Vec::new(),
            genesis_identifier,
        }
    }

    /// Visit the blocks of the current main branch, oldest first
    pub fn for_each_main(&self, mut visit: impl FnMut(&D)) {
        let mut cursor = match &self.head {
            Some(head) => Rc::clone(head),
            None => return,
        };

        loop {
            {
                let node = cursor.borrow();
                if let Some(block) = node.block.as_ref() {
                    visit(block);
                }
                if node.next.is_none() {
                    break;
                }
            }

            let next = Node::longest_right(Rc::clone(&cursor));
            if Rc::ptr_eq(&next, &cursor) {
                break;
            }
            cursor = next;
        }
    }

    /// Identifier of the current head, i.e. the next block to be popped
    pub fn next_id(&self) -> I {
        match &self.head {
            Some(head) => head.borrow().block.as_ref().unwrap().get_block_id(),
            None => self.genesis_identifier,
        }
    }

    fn longest_chain(&self) -> Option<Rc<RefCell<Node<D>>>> {
        match &self.head {
            Some(head) => Some(Node::longest_right(Rc::clone(head))),
            None => None,
        }
    }

    pub fn longest_chain_depth(&self) -> u32 {
        match &self.head {
            Some(head) => Node::depth(head.clone()),
            None => 0,
        }
    }

    fn tails(&self) -> Vec<Rc<RefCell<Node<D>>>> {
        match &self.head {
            Some(head) => Node::extract_right(Rc::clone(head))
                .iter()
                .filter(|node| node.borrow().next.is_none())
                .map(|node| Rc::clone(node))
                .collect(),
            None => vec![],
        }
    }

    pub fn orphans(&self) -> usize {
        self.orphans.values().map(Vec::len).sum()
    }

    /// Blocks discarded by `pop_head` since the last call: the stale
    /// side of settled branch points, each parent before its children
    pub fn take_discarded(&mut self) -> Vec<D> {
        std::mem::take(&mut self.discarded)
    }

    pub fn insert(&mut self, block: D) {
        let mut pending = match self.attach(block) {
            Some(block_hash) => vec![block_hash],
            None => return,
        };

        // Attach pending orphan descendants iteratively (a recursive
        // resolution could overflow the stack on long orphan chains)
        while let Some(parent_hash) = pending.pop() {
            if let Some(orphans) = self.orphans.remove(&parent_hash) {
                for orphan in orphans {
                    if let Some(block_hash) = self.attach(orphan) {
                        pending.push(block_hash);
                    }
                }
            }
        }
    }

    /// Attach a block to the chain, or store it as an orphan.
    /// Returns the block id if it was attached.
    fn attach(&mut self, block: D) -> Option<I> {
        let block_hash = block.get_block_id();
        let prev_hash = block.get_block_prev_id();

        // This is the genesis block
        if self.head.is_none() && prev_hash == self.genesis_identifier {
            let node = Rc::new(RefCell::new(Node {
                block: Some(block),
                prev: None,
                next: None,
            }));

            self.nodes.insert(block_hash, node.clone());
            self.head = Some(node);

            return Some(block_hash);
        }

        let parent_node = match self.nodes.get(&prev_hash) {
            // If the new block is an orphan, add it to the orphans list
            None => {
                self.orphans.entry(prev_hash).or_default().push(block);
                return None;
            }
            Some(parent_node) => Rc::clone(parent_node),
        };

        let node = Rc::new(RefCell::new(Node {
            block: Some(block),
            prev: Some(Rc::downgrade(&parent_node)),
            next: None,
        }));

        // Add the new node to the parent's next list
        parent_node.borrow_mut().add_next(node.clone());

        // Add the new node to the nodes map
        self.nodes.insert(block_hash, node);

        Some(block_hash)
    }

    /// Pop head: remove the head of the chain and return it
    /// If the chain is empty, return None
    /// If the chain has only one block, return the block and set the head to None
    /// If the head has a single next node, set the head to the next node
    /// If the head has multiple next nodes, remove all nodes except the next node from the longest chain
    pub fn pop_head(&mut self) -> Option<D> {
        let longest_chain = self.longest_chain()?;

        let tail = longest_chain.clone();
        let chain = Node::extract_left(tail);

        let mut head_node = chain.first()?.borrow_mut();
        let head = head_node.block.take().unwrap();
        let head_id = head.get_block_id();

        let next = chain.get(1).map(|node| (*node).clone());

        // Remove the head from the nodes map
        self.nodes.remove(&head_id);

        let next = match next {
            Some(next) => next,
            None => {
                self.head = None;
                return Some(head);
            }
        };

        // Update the prev node for the next head
        next.borrow_mut().prev = None;

        // Update the new head
        match head_node.next.as_ref() {
            None => {
                self.head = None;
                return Some(head);
            }
            Some(next_nodes) => match next_nodes {
                NextNode::Single(node) => {
                    self.head = Some(node.clone());
                    return Some(head);
                }
                NextNode::Multiple(nodes) => {
                    for node in nodes.iter() {
                        // Keep the branch we are following
                        if Rc::ptr_eq(&next, node) {
                            continue;
                        }

                        // Discard the losing fork, keeping its blocks
                        // for the caller (stale-block reporting)
                        for node in Node::extract_right(Rc::clone(node)).iter() {
                            let block = node.borrow_mut().block.take().unwrap();
                            self.nodes.remove(&block.get_block_id());
                            self.discarded.push(block);
                        }
                    }

                    self.head = Some(next);
                    return Some(head);
                }
            },
        }
    }
}

impl<I: Ord + Copy + Display, D: GetBlockIds<I>> std::fmt::Display for Chain<I, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let tails = self.tails();

        for tail in tails {
            let nodes = Node::extract_left(tail.clone());

            writeln!(
                f,
                "{}",
                nodes
                    .iter()
                    .map(|node| node
                        .borrow()
                        .block
                        .as_ref()
                        .unwrap()
                        .get_block_id()
                        .to_string())
                    .collect::<Vec<String>>()
                    .join(" -> ")
            )?;
        }

        if self.orphans.is_empty() {
            return Ok(());
        }

        write!(f, "Orphans: ")?;
        for (_, blocks) in self.orphans.iter() {
            for data in blocks.iter() {
                write!(
                    f,
                    "{} (prev: {}),",
                    data.get_block_id(),
                    data.get_block_prev_id()
                )?;
            }
        }
        write!(f, "\n")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct Block {
        block_id: &'static str,
        prev_block_id: &'static str,
    }

    impl Block {
        fn new(block_id: &'static str, prev_block_id: &'static str) -> Block {
            Block {
                block_id,
                prev_block_id,
            }
        }
    }

    impl GetBlockIds<&'static str> for Block {
        fn get_block_id(&self) -> &'static str {
            self.block_id
        }

        fn get_block_prev_id(&self) -> &'static str {
            self.prev_block_id
        }
    }

    #[test]
    fn test_chain() {
        let mut chain = Chain::new("genesis-identifier");

        let block1 = Block::new("1", "genesis-identifier");
        let block2 = Block::new("2", "1");
        let block3b = Block::new("3b", "2");
        let block3 = Block::new("3a", "2");
        let block4 = Block::new("4", "3a");
        let block5 = Block::new("5", "4");

        chain.insert(block1);
        chain.insert(block2);
        chain.insert(block3);
        chain.insert(block4);
        chain.insert(block3b);
        chain.insert(block5);

        assert_eq!(chain.orphans.len(), 0);
        assert_eq!(chain.longest_chain_depth(), 5);

        println!("Chains: \n{}", chain);

        let block = chain.pop_head();
        println!(
            "Pop head {}, new head {}",
            block.as_ref().unwrap().block_id,
            chain
                .head
                .as_ref()
                .unwrap()
                .borrow()
                .block
                .as_ref()
                .unwrap()
                .block_id
        );
        assert_eq!(block.unwrap().block_id, "1");
        assert_eq!(chain.longest_chain_depth(), 4);
        println!("Chains: \n{}", chain);

        let block = chain.pop_head();
        println!(
            "Pop head {}, new head {}",
            block.as_ref().unwrap().block_id,
            chain
                .head
                .as_ref()
                .unwrap()
                .borrow()
                .block
                .as_ref()
                .unwrap()
                .block_id
        );
        assert_eq!(block.unwrap().block_id, "2");
        assert_eq!(chain.longest_chain_depth(), 3);
        println!("Chains: \n{}", chain);

        // Insert orphan
        chain.insert(Block::new("7", "6"));
        assert_eq!(chain.orphans.len(), 1);
        assert_eq!(chain.longest_chain_depth(), 3);
        println!("Chains: \n{}", chain);

        // Insert orphan parent
        chain.insert(Block::new("6", "5"));
        assert_eq!(chain.orphans.len(), 0);
        assert_eq!(chain.longest_chain_depth(), 5);
        println!("Chains: \n{}", chain);
    }

    #[test]
    fn test_competing_orphans() {
        let mut chain = Chain::new("genesis-identifier");

        chain.insert(Block::new("1", "genesis-identifier"));

        // Two competing blocks share the same missing parent "2"
        chain.insert(Block::new("3a", "2"));
        chain.insert(Block::new("3b", "2"));
        assert_eq!(chain.orphans(), 2);

        // Child of one of the competing orphans
        chain.insert(Block::new("4", "3a"));
        assert_eq!(chain.orphans(), 3);

        // Parent arrives: all pending children must be attached
        chain.insert(Block::new("2", "1"));
        assert_eq!(chain.orphans(), 0);

        // 1 -> 2 -> 3a -> 4 (3b is a shorter fork)
        assert_eq!(chain.longest_chain_depth(), 4);

        let stale = Rc::downgrade(chain.nodes.get("3b").unwrap());

        assert_eq!(chain.pop_head().unwrap().block_id, "1");
        assert!(chain.take_discarded().is_empty());

        // Popping "2" discards the losing fork "3b"
        assert_eq!(chain.pop_head().unwrap().block_id, "2");
        assert!(stale.upgrade().is_none(), "discarded fork must be freed");

        let discarded = chain.take_discarded();
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].block_id, "3b");
        assert!(chain.take_discarded().is_empty(), "drained once");

        assert_eq!(chain.pop_head().unwrap().block_id, "3a");
        assert_eq!(chain.pop_head().unwrap().block_id, "4");
        assert!(chain.pop_head().is_none());
    }

    #[test]
    fn test_discarded_fork_order() {
        let mut chain = Chain::new("genesis-identifier");

        chain.insert(Block::new("1", "genesis-identifier"));
        chain.insert(Block::new("2", "1"));

        // Losing fork of 3 blocks, with a split at its second block
        chain.insert(Block::new("3b", "2"));
        chain.insert(Block::new("4b", "3b"));
        chain.insert(Block::new("5b", "4b"));
        chain.insert(Block::new("5c", "4b"));

        // Winning branch, one block longer
        for (id, prev) in [("3a", "2"), ("4a", "3a"), ("5a", "4a"), ("6a", "5a")] {
            chain.insert(Block::new(id, prev));
        }

        assert_eq!(chain.pop_head().unwrap().block_id, "1");
        assert_eq!(chain.pop_head().unwrap().block_id, "2");

        let ids: Vec<&str> = chain.take_discarded().iter().map(|b| b.block_id).collect();
        assert_eq!(ids, ["3b", "4b", "5b", "5c"]);
    }
}
