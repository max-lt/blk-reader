use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::rc::Rc;

pub trait GetBlockIds<Identifier> {
    fn get_block_id(&self) -> Identifier;
    fn get_block_prev_id(&self) -> Identifier;
}

#[derive(Debug, Clone)]
pub struct Node<Data> {
    pub block: Option<Data>,
    pub prev: Option<Rc<RefCell<Node<Data>>>>,
    pub next: Option<Vec<Rc<RefCell<Node<Data>>>>>,
}

#[derive(Debug, Clone)]
pub struct Chain<I, D> {
    head: Option<Rc<RefCell<Node<D>>>>,
    tails: BTreeMap<I, Rc<RefCell<Node<D>>>>,
    nodes: BTreeMap<I, Rc<RefCell<Node<D>>>>,
    orphans: BTreeMap<I, D>,
    genesis_identifier: I,
}

impl<D> Node<D> {
    fn add_next(&mut self, node: Rc<RefCell<Node<D>>>) {
        match &mut self.next {
            Some(next) => next.push(node),
            None => self.next = Some(vec![node]),
        }
    }

    fn depth(&self) -> u32 {
        match &self.prev {
            Some(prev) => prev.borrow().depth() + 1,
            None => 1,
        }
    }

    // Extract all nodes recursively from the current node to the head
    fn extract_left(node: Rc<RefCell<Node<D>>>) -> Vec<Rc<RefCell<Node<D>>>> {
        match &node.borrow().prev {
            Some(prev) => {
                let mut nodes = Node::extract_left(Rc::clone(prev));
                nodes.push(Rc::clone(&node));
                nodes
            }
            None => vec![Rc::clone(&node)],
        }
    }

    // Extract all nodes recursively from the current node to the tails
    fn extract_right(node: Rc<RefCell<Node<D>>>) -> Vec<Rc<RefCell<Node<D>>>> {
        match &node.borrow().next {
            Some(next) => {
                let mut nodes = vec![Rc::clone(&node)];
                for n in next.iter() {
                    nodes.extend(Node::extract_right(Rc::clone(n)));
                }
                nodes
            }
            None => vec![Rc::clone(&node)],
        }
    }
}

impl<I: PartialEq + Ord + Copy + Display, D: Clone + GetBlockIds<I>> Chain<I, D> {
    pub fn new(genesis_identifier: I) -> Chain<I, D> {
        Chain {
            head: None,
            orphans: BTreeMap::new(),
            nodes: BTreeMap::new(),
            tails: BTreeMap::new(),
            genesis_identifier,
        }
    }

    fn longest_chain(&self) -> Option<Rc<RefCell<Node<D>>>> {
        let mut max_depth = 0;
        let mut longest = None;

        for (_id, node) in self.tails.iter() {
            let depth = node.borrow().depth();
            if depth > max_depth {
                max_depth = depth;
                longest = Some(node.clone());
            }
        }

        return longest;
    }

    pub fn longest_chain_depth(&self) -> u32 {
        let mut max_depth = 0;

        for (_id, node) in self.tails.iter() {
            let depth = node.borrow().depth();
            if depth > max_depth {
                max_depth = depth;
            }
        }

        return max_depth;
    }

    pub fn orphans(&self) -> usize {
        self.orphans.len()
    }

    pub fn insert(&mut self, block: D) {
        let block_hash = block.get_block_id();
        let prev_hash = block.get_block_prev_id();

        // println!("Insert block  {} (prev: {})", block_hash, prev_hash);

        // This is the genesis block
        if self.head.is_none() && prev_hash == self.genesis_identifier {
            let node = Rc::new(RefCell::new(Node {
                block: Some(block),
                prev: None,
                next: None,
            }));

            self.tails.insert(block_hash, node.clone());
            self.nodes.insert(block_hash, node.clone());
            self.head = Some(node);

            return;
        }

        match self.nodes.get_mut(&prev_hash) {
            // If the new block is an orphan, add it to the orphans list and return
            None => {
                self.orphans.insert(prev_hash, block);
                return;
            }
            // If the new block is a child of a parent node, add it to the parent's next list
            Some(parent_node) => {
                let node = Rc::new(RefCell::new(Node {
                    block: Some(block),
                    prev: Some(parent_node.clone()),
                    next: None,
                }));

                // node.prev = Some(parent_node.clone());

                // Add the new node to the parent's next list
                parent_node.borrow_mut().add_next(node.clone());

                // If the parent node was a tail, it's no longer a tail
                self.tails.remove(&prev_hash);

                // Update the new node as a tail
                self.tails.insert(block_hash, node.clone());

                // Add the new node to the nodes map
                self.nodes.insert(block_hash, node.clone());

                node
            }
        };

        // // We inserted a new block, check if we can insert any orphans
        match self.orphans.remove(&block_hash) {
            Some(orphan) => {
                // println!(
                //     "Insert orphan {} (prev: {})",
                //     orphan.get_block_id(),
                //     block_hash
                // );
                self.insert(orphan);
            }
            None => {}
        };
    }

    /// Pop head
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

        // Remove the head from the tails map
        self.tails.remove(&head_id);

        let next = match next {
            Some(next) => next,
            None => {
                self.head = None;
                return Some(head);
            }
        };

        // Update the new head
        next.borrow_mut().prev = None;

        match head_node.next.as_ref() {
            None => {
                self.head = None;
                return Some(head);
            }
            Some(next_nodes) => match next_nodes.len() {
                0 => {
                    self.head = None;
                    return Some(head);
                }
                1 => {
                    self.head = Some(next);
                    return Some(head);
                }
                _ => {
                    for node in next_nodes.iter() {
                        // Continue if node is next
                        if Rc::ptr_eq(&next, node) {
                            println!(
                                "Continue, ignoring {}",
                                node.borrow().block.as_ref().unwrap().get_block_id()
                            );
                            continue;
                        }

                        let nodes = Node::extract_right(Rc::clone(node));
                        println!("Removing nodes: {}", nodes.len());
                        for node in nodes.iter() {
                            let node_id = node.borrow().block.as_ref().unwrap().get_block_id();
                            println!("Removing node {}", node_id);
                            self.tails.remove(&node_id);
                            self.nodes.remove(&node_id);
                        }
                    }

                    self.head = Some(next);
                    return Some(head);
                }
            },
        }
    }
}

impl<I: std::fmt::Display, D: GetBlockIds<I>> std::fmt::Display for Chain<I, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        println!("nTails: {}", self.tails.len());

        for (_hash, tail) in self.tails.iter() {
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
        for (_, data) in self.orphans.iter() {
            write!(
                f,
                "{} (prev: {}),",
                data.get_block_id(),
                data.get_block_prev_id()
            )?;
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
}