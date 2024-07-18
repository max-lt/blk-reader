use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::rc::Rc;

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
    prev: Option<Rc<RefCell<Node<Data>>>>,
    next: Option<NextNode<Data>>,
}

#[derive(Debug, Clone)]
pub struct Chain<I, D> {
    head: Option<Rc<RefCell<Node<D>>>>,
    nodes: BTreeMap<I, Rc<RefCell<Node<D>>>>,
    orphans: BTreeMap<I, D>,
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
            Some(next) => match next {
                NextNode::Single(next) => {
                    let mut nodes = Node::extract_right(Rc::clone(next));
                    nodes.push(Rc::clone(&node));
                    nodes
                }
                NextNode::Multiple(nodes) => {
                    let mut all_nodes = vec![Rc::clone(&node)];
                    for next in nodes.iter() {
                        let mut nodes = Node::extract_right(Rc::clone(next));
                        all_nodes.append(&mut nodes);
                    }
                    all_nodes
                }
            },
            None => vec![Rc::clone(&node)],
        }
    }

    /// Extract the tail of longest chain from the current node to the right
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

impl<I: PartialEq + Ord + Copy + Display, D: Clone + GetBlockIds<I>> Chain<I, D> {
    pub fn new(genesis_identifier: I) -> Chain<I, D> {
        Chain {
            head: None,
            orphans: BTreeMap::new(),
            nodes: BTreeMap::new(),
            genesis_identifier,
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
        self.orphans.len()
    }

    pub fn insert(&mut self, block: D) {
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

                // Add the new node to the parent's next list
                parent_node.borrow_mut().add_next(node.clone());

                // Add the new node to the nodes map
                self.nodes.insert(block_hash, node.clone());

                node
            }
        };

        // We inserted a new block, check if we can insert any orphans
        match self.orphans.remove(&block_hash) {
            Some(orphan) => self.insert(orphan),
            None => {}
        };
    }

    /// Pop head: remove the head of the longest chain and return it
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

        // Update the new head
        next.borrow_mut().prev = None;

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

impl<I: PartialEq + Ord + Copy + Display, D: Clone + GetBlockIds<I>> std::fmt::Display for Chain<I, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let tails = self.tails();

        println!("nTails: {}", tails.len());

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
