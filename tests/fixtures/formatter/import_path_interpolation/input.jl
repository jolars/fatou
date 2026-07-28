for op in (:+, :*, :÷)
    @eval import Base.$(op)
    @eval $(op)(::Foo, ::Foo) = Foo()
end
import a.$b
using A.$(b): c
