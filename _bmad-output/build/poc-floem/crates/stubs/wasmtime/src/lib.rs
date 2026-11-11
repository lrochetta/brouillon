use std::any::Any;

pub struct Engine;

impl Engine {
    pub fn new(_config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self)
    }
}

pub struct Config;

impl Config {
    pub fn new() -> Self {
        Self
    }
    pub fn strategy(&mut self, _strategy: Strategy) -> &mut Self {
        self
    }
    pub fn cranelift_opt_level(&mut self, _level: OptLevel) -> &mut Self {
        self
    }
}

pub enum Strategy {
    Cranelift,
}

pub enum OptLevel {
    Speed,
}

pub struct Store<T> {
    data: T,
}

impl<T> Store<T> {
    pub fn new(_engine: &Engine, data: T) -> Self {
        Self { data }
    }
    pub fn into_data(self) -> T {
        self.data
    }
}

pub struct Module;

impl Module {
    pub fn from_file(_engine: &Engine, _path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self)
    }
}

pub struct Instance;

impl Instance {
    pub fn get_typed_func<Params, Results>(
        &self,
        _store: &mut Store<impl Any>,
        _name: &str,
    ) -> Result<TypedFunc<Params, Results>, Box<dyn std::error::Error>> {
        Err("stub instance".into())
    }
}

pub struct TypedFunc<Params, Results> {
    _marker: std::marker::PhantomData<(Params, Results)>,
}

impl<Params, Results> TypedFunc<Params, Results> {
    pub fn call(
        &self,
        _store: &mut Store<impl Any>,
        _params: Params,
    ) -> Result<Results, Box<dyn std::error::Error>> {
        panic!("stub call")
    }
}

pub struct Linker<T> {
    _marker: std::marker::PhantomData<T>,
}

impl<T> Linker<T> {
    pub fn new(_engine: &Engine) -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
    pub fn func_wrap<F>(
        &mut self,
        _module: &str,
        _name: &str,
        _func: F,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    pub fn instantiate(
        &self,
        _store: &mut Store<T>,
        _module: &Module,
    ) -> Result<Instance, Box<dyn std::error::Error>> {
        Ok(Instance)
    }
}

pub struct Caller<'a, T> {
    _marker: std::marker::PhantomData<&'a T>,
}

impl<'a, T> Caller<'a, T> {
    pub fn data(&self) -> &T {
        panic!("stub Caller::data")
    }
    pub fn data_mut(&mut self) -> &mut T {
        panic!("stub Caller::data_mut")
    }
    pub fn get_export(&self, _name: &str) -> Option<Extern> {
        None
    }
}

pub enum Extern {
    Memory(Memory),
}

#[derive(Clone)]
pub struct Memory;

impl Memory {
    pub fn data_size(&self, _store: &impl ?Sized) -> usize {
        0
    }
    pub fn read(
        &self,
        _store: &impl ?Sized,
        _offset: usize,
        _buffer: &mut [u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    pub fn write(
        &self,
        _store: &mut impl ?Sized,
        _offset: usize,
        _buffer: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
