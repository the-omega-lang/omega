#[derive(Debug, Clone)]
pub enum ObjectType {
    ELF,
}

pub enum ObjectBuilderError {}

pub trait ObjectBuilder: Sized {
    fn object_type(&mut self) -> ObjectType;
    fn process(&mut self) -> Self;
    fn build(self) -> Self;
}
